//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct, indexed graph execution over durable records. Handles contain no
//! vertex, edge, membership, or adjacency replicas.

mod catalog;
mod overlay;
pub(crate) mod storage;
#[cfg(test)]
mod tests;
mod trait_impl;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::{Edge, Vertex};
use uqa_storage::{CatalogFacade, GraphEntityFilter, GraphEntityKind, PersistentStorageBackend};

use crate::{
    graphid_label_id, make_graphid, Direction, GraphLabelInfo, GraphLabelRegistry, GraphStore,
    GraphStoreError, GraphStoreResult, LabelKind,
};
use storage::GraphStorage;

const ID_PAGE_SIZE: usize = 256;

/// A session-bound durable graph handle. Cloning duplicates only the handle;
/// atomic rollback is provided by `GraphStore::transaction`, not by Clone.
pub struct PersistentGraphStore {
    storage: Arc<dyn GraphStorage>,
    resource: Option<Arc<dyn Send + Sync>>,
}

impl Clone for PersistentGraphStore {
    fn clone(&self) -> Self {
        Self {
            storage: Arc::clone(&self.storage),
            resource: self.resource.clone(),
        }
    }
}

impl std::fmt::Debug for PersistentGraphStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentGraphStore")
            .finish_non_exhaustive()
    }
}

impl PersistentGraphStore {
    /// The catalog and backend must belong to the same storage session.
    #[must_use]
    pub fn from_catalog(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> Self {
        Self::from_storage(Arc::new(catalog::CatalogGraphStorage { catalog, backend }))
    }

    pub(crate) fn from_storage(storage: Arc<dyn GraphStorage>) -> Self {
        Self {
            storage,
            resource: None,
        }
    }

    /// Keep a physical snapshot's resource owner alive with every cloned
    /// handle (for example an encrypted temporary snapshot directory).
    pub fn retain_resource(mut self, resource: Arc<dyn Send + Sync>) -> Self {
        self.resource = Some(resource);
        self
    }

    /// Read from a pinned durable snapshot, with this transaction's writes
    /// overlaid by identity. No snapshot entities are copied into memory.
    pub fn with_read_snapshot(&self, snapshot: &Self) -> Self {
        Self::from_storage(Arc::new(overlay::OverlayGraphStorage::new(
            self.clone(),
            snapshot.clone(),
        )))
    }

    /// A mutation candidate copies only its changed-id sets. Previously
    /// published candidates are immutable rollback/savepoint checkpoints.
    pub fn fork_for_mutation(&self) -> Self {
        Self {
            storage: self
                .storage
                .fork_overlay()
                .unwrap_or_else(|| Arc::clone(&self.storage)),
            resource: self.resource.clone(),
        }
    }

    /// Reuse a transaction's original pinned reader when it has no graph
    /// changes. A cursor can retain this owner independently of COMMIT.
    pub fn unmodified_read_snapshot(&self) -> Option<Self> {
        self.storage.unmodified_read_snapshot()
    }

    pub(crate) fn transaction_mapped<T, E>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, E>,
        map_error: impl Fn(GraphStoreError) -> E,
    ) -> Result<T, E>
    where
        E: std::fmt::Display,
    {
        let mut checkpoint = self.storage.begin_write().map_err(&map_error)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self)));
        match result {
            Ok(Ok(value)) => match checkpoint.commit() {
                Ok(()) => Ok(value),
                Err(error) => {
                    checkpoint.rollback().map_err(|rollback| {
                        map_error(GraphStoreError::Storage(format!(
                            "{error}; rollback failed: {rollback}"
                        )))
                    })?;
                    Err(map_error(error))
                }
            },
            Ok(Err(error)) => {
                checkpoint.rollback().map_err(|rollback| {
                    map_error(GraphStoreError::Storage(format!(
                        "{error}; rollback failed: {rollback}"
                    )))
                })?;
                Err(error)
            }
            Err(panic) => {
                drop(checkpoint);
                std::panic::resume_unwind(panic)
            }
        }
    }

    fn require_graph(&self, graph: &str) -> GraphStoreResult<()> {
        if self.storage.has_graph(graph)? {
            Ok(())
        } else {
            Err(GraphStoreError::UnknownGraph(graph.to_owned()))
        }
    }

    fn for_each_id(
        &self,
        filter: GraphEntityFilter<'_>,
        mut visit: impl FnMut(u64) -> GraphStoreResult<()>,
    ) -> GraphStoreResult<()> {
        let mut after = None;
        loop {
            let ids = self.storage.ids(filter, after, ID_PAGE_SIZE)?;
            if ids.is_empty() {
                return Ok(());
            }
            after = ids.last().copied();
            for id in ids {
                visit(id)?;
            }
        }
    }

    fn collect_ids(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<BTreeSet<u64>> {
        let mut ids = BTreeSet::new();
        self.for_each_id(filter, |id| {
            ids.insert(id);
            Ok(())
        })?;
        Ok(ids)
    }

    fn require_vertex(&self, id: u64) -> GraphStoreResult<Vertex> {
        self.storage.vertex(id)?.ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!(
                "graph membership references missing vertex {id}"
            ))
        })
    }

    fn require_edge(&self, id: u64, graph: &str) -> GraphStoreResult<Edge> {
        let edge = self.storage.edge(id)?.ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!("graph {graph:?} references missing edge {id}"))
        })?;
        for endpoint in [edge.source_id, edge.target_id] {
            if self
                .storage
                .has_membership(GraphEntityKind::Vertex, endpoint, graph)?
            {
                if self.storage.vertex(endpoint)?.is_some() {
                    continue;
                }
            } else if self
                .storage
                .registry(graph)?
                .dropped_label_ids
                .contains(&graphid_label_id(endpoint))
            {
                continue;
            }
            return Err(GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} edge {id} references missing endpoint {endpoint}"
            )));
        }
        Ok(edge)
    }

    fn detach_entity(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.storage.detach(kind, id, graph)?;
        if self.storage.memberships(kind, id)?.is_empty() {
            match kind {
                GraphEntityKind::Vertex => self.storage.delete_vertex(id)?,
                GraphEntityKind::Edge => self.storage.delete_edge(id)?,
            }
        }
        Ok(())
    }

    fn reserve_id(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<()> {
        let next = id.checked_add(1).ok_or_else(|| {
            GraphStoreError::IdExhausted(format!("{} id counter overflow", kind.as_str()))
        })?;
        let previous = self.next_counter(kind)?;
        self.storage.save_counter(kind, next.max(previous))?;
        Ok(())
    }

    fn next_counter(&self, kind: GraphEntityKind) -> GraphStoreResult<u64> {
        if let Some(next) = self.storage.counter(kind)? {
            return Ok(next);
        }
        self.storage
            .max_id(kind)?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                GraphStoreError::IdExhausted(format!("{} id counter overflow", kind.as_str()))
            })
    }

    fn allocate_counter(&mut self, kind: GraphEntityKind) -> GraphStoreResult<u64> {
        self.transaction(|store| {
            let id = store.next_counter(kind)?;
            let next = id.checked_add(1).ok_or_else(|| {
                GraphStoreError::IdExhausted(format!("{} id counter overflow", kind.as_str()))
            })?;
            store.storage.save_counter(kind, next)?;
            Ok(id)
        })
    }

    fn allocate_label_id(
        &mut self,
        label: &str,
        graph: &str,
        kind: LabelKind,
    ) -> GraphStoreResult<u64> {
        self.transaction(|store| {
            store.require_graph(graph)?;
            let mut registry = store.storage.registry(graph)?;
            let label_id = registry.label_id(label, kind)?;
            let id = make_graphid(label_id, registry.next_sequence(label_id)?)?;
            store.storage.save_registry(graph, &registry)?;
            Ok(id)
        })
    }

    pub fn label_registry(&self, graph: &str) -> GraphStoreResult<GraphLabelRegistry> {
        self.require_graph(graph)?;
        self.storage.registry(graph)
    }

    pub fn graph_labels(&self, graph: &str) -> GraphStoreResult<Vec<GraphLabelInfo>> {
        Ok(self.label_registry(graph)?.labels())
    }

    pub fn graph_label_kind(
        &self,
        graph: &str,
        label: &str,
    ) -> GraphStoreResult<Option<LabelKind>> {
        Ok(self.label_registry(graph)?.label_kind(label))
    }

    pub fn import_label_registry(
        &mut self,
        graph: &str,
        registry: &GraphLabelRegistry,
    ) -> GraphStoreResult<()> {
        self.transaction(|store| {
            let mut combined = store.label_registry(graph)?;
            combined.merge(registry);
            store.storage.save_registry(graph, &combined)
        })
    }

    /// One-time legacy migration, streaming entity observations without
    /// retaining their properties or an entity map in the engine.
    pub fn rebuild_label_registry_from_ids(&mut self, graph: &str) -> GraphStoreResult<()> {
        self.transaction(|store| {
            let mut registry = store.label_registry(graph)?;
            for kind in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
                store.for_each_id(GraphEntityFilter::new(kind, Some(graph)), |id| {
                    match kind {
                        GraphEntityKind::Vertex => registry.observe(
                            &store.require_vertex(id)?.label,
                            id,
                            LabelKind::Vertex,
                        ),
                        GraphEntityKind::Edge => registry.observe(
                            &store.require_edge(id, graph)?.label,
                            id,
                            LabelKind::Edge,
                        ),
                    }
                    Ok(())
                })?;
            }
            store.storage.save_registry(graph, &registry)
        })
    }

    pub fn create_label(
        &mut self,
        graph: &str,
        label: &str,
        kind: LabelKind,
    ) -> GraphStoreResult<Option<u32>> {
        self.transaction(|store| {
            let mut registry = store.label_registry(graph)?;
            let result = registry.register_label(label, kind)?;
            if result.is_some() {
                store.storage.save_registry(graph, &registry)?;
            }
            Ok(result)
        })
    }

    pub fn drop_label(
        &mut self,
        graph: &str,
        label: &str,
    ) -> GraphStoreResult<Option<(u32, LabelKind)>> {
        self.transaction(|store| {
            let mut registry = store.label_registry(graph)?;
            let Some(kind) = registry.label_kind(label) else {
                return Ok(None);
            };
            let default = label == kind.default_label_name();
            let id = if default {
                if let Some(dependent) = registry
                    .labels
                    .keys()
                    .find(|name| registry.label_kind(name) == Some(kind))
                {
                    return Err(GraphStoreError::InvalidMutation(format!(
                        "cannot drop default label {label} while label {dependent} depends on it"
                    )));
                }
                kind.default_label_id()
            } else {
                *registry.labels.get(label).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!(
                        "graph {graph:?} label {label:?} has no registry id"
                    ))
                })?
            };
            let entity_kind = match kind {
                LabelKind::Vertex => GraphEntityKind::Vertex,
                LabelKind::Edge => GraphEntityKind::Edge,
            };
            let mut filter = GraphEntityFilter::new(entity_kind, Some(graph));
            if !default {
                filter.label = Some(label);
            }
            // AGE label removal intentionally preserves incident edge rows;
            // the tombstone distinguishes these from corrupt endpoints.
            store.for_each_id(filter, |entity_id| {
                if !default || graphid_label_id(entity_id) == id {
                    store.detach_entity(entity_kind, entity_id, graph)?;
                }
                Ok(())
            })?;
            registry.remove_label(label);
            store.storage.save_registry(graph, &registry)?;
            Ok(Some((id, kind)))
        })
    }

    pub fn rename_graph(&mut self, from: &str, to: &str) -> GraphStoreResult<()> {
        if from == to {
            return Ok(());
        }
        self.transaction(|store| {
            store.require_graph(from)?;
            if store.storage.has_graph(to)? {
                return Err(GraphStoreError::InvalidMutation(format!(
                    "graph {to:?} already exists"
                )));
            }
            store.storage.create_graph(to)?;
            store
                .storage
                .save_registry(to, &store.label_registry(from)?)?;
            for kind in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
                store.for_each_id(GraphEntityFilter::new(kind, Some(from)), |id| {
                    store.storage.attach(kind, id, to)?;
                    store.storage.detach(kind, id, from)
                })?;
            }
            store.storage.delete_graph(from)
        })
    }

    fn algebra(
        &mut self,
        first: &str,
        second: Option<&str>,
        target: &str,
        keep: impl Fn(bool) -> bool,
        include_second: bool,
    ) -> GraphStoreResult<()> {
        self.transaction(|store| {
            store.require_graph(first)?;
            if let Some(second) = second {
                store.require_graph(second)?;
            }
            store.create_graph(target)?;
            for kind in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
                store.for_each_id(GraphEntityFilter::new(kind, Some(first)), |id| {
                    let common = match second {
                        Some(second) => store.storage.has_membership(kind, id, second)?,
                        None => false,
                    };
                    if keep(common) {
                        store.storage.attach(kind, id, target)?;
                    }
                    Ok(())
                })?;
                if let Some(second) = second.filter(|_| include_second) {
                    store.for_each_id(GraphEntityFilter::new(kind, Some(second)), |id| {
                        store.storage.attach(kind, id, target)
                    })?;
                }
            }
            let mut registry = store.label_registry(target)?;
            registry.merge(&store.label_registry(first)?);
            if let Some(second) = second {
                registry.merge(&store.label_registry(second)?);
            }
            store.storage.save_registry(target, &registry)
        })
    }
}
