//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MVCC graph reads over a pinned physical snapshot plus transaction-local
//! changed identities. Entity payloads remain in their respective backends.

use super::storage::{GraphStorage, GraphWriteTransaction};
use super::{GraphLabelRegistry, GraphStoreError, GraphStoreResult, PersistentGraphStore};
use parking_lot::Mutex;
use std::collections::BTreeSet;
use std::sync::Arc;
use uqa_core::{Edge, Vertex};
use uqa_storage::{GraphEntityFilter, GraphEntityKind};

#[derive(Clone, Default)]
struct Changes {
    vertices: BTreeSet<u64>,
    edges: BTreeSet<u64>,
    graphs: BTreeSet<String>,
    registries: BTreeSet<String>,
}
impl Changes {
    fn ids(&self, kind: GraphEntityKind) -> &BTreeSet<u64> {
        match kind {
            GraphEntityKind::Vertex => &self.vertices,
            GraphEntityKind::Edge => &self.edges,
        }
    }
    fn ids_mut(&mut self, kind: GraphEntityKind) -> &mut BTreeSet<u64> {
        match kind {
            GraphEntityKind::Vertex => &mut self.vertices,
            GraphEntityKind::Edge => &mut self.edges,
        }
    }
}
#[derive(Default)]
struct OverlayState {
    changes: Changes,
    write_depth: usize,
}

pub(super) struct OverlayGraphStorage {
    write: PersistentGraphStore,
    read: PersistentGraphStore,
    state: Arc<Mutex<OverlayState>>,
}
impl OverlayGraphStorage {
    pub(super) fn new(write: PersistentGraphStore, read: PersistentGraphStore) -> Self {
        Self {
            write,
            read,
            state: Arc::default(),
        }
    }

    fn changed(&self, kind: GraphEntityKind, id: u64) -> bool {
        self.state.lock().changes.ids(kind).contains(&id)
    }
    fn mark(&self, kind: GraphEntityKind, id: u64) {
        self.state.lock().changes.ids_mut(kind).insert(id);
    }
    fn conflict(kind: &str, id: impl std::fmt::Display) -> GraphStoreError {
        GraphStoreError::SerializationFailure(format!(
            "{kind} {id} changed since the transaction snapshot"
        ))
    }
    fn check_entity(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<()> {
        if self.changed(kind, id) {
            return Ok(());
        }
        let equal = match kind {
            GraphEntityKind::Vertex => {
                self.read.storage.vertex(id)? == self.write.storage.vertex(id)?
            }
            GraphEntityKind::Edge => self.read.storage.edge(id)? == self.write.storage.edge(id)?,
        };
        if !equal
            || self.read.storage.memberships(kind, id)?
                != self.write.storage.memberships(kind, id)?
        {
            return Err(Self::conflict(kind.as_str(), id));
        }
        Ok(())
    }
    fn check_vertex_adjacency(&self, id: u64, graph: &str) -> GraphStoreResult<()> {
        for outgoing in [true, false] {
            let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
            if outgoing {
                filter.source = Some(id);
            } else {
                filter.target = Some(id);
            }
            self.check_identity_stream(filter)?;
        }
        Ok(())
    }
    fn check_edge_endpoints(&self, edge: &Edge, graph: &str) -> GraphStoreResult<()> {
        for endpoint in [edge.source_id, edge.target_id] {
            let missing =
                !self
                    .write
                    .storage
                    .has_membership(GraphEntityKind::Vertex, endpoint, graph)?
                    || self.write.storage.vertex(endpoint)?.is_none();
            if missing
                && !self
                    .write
                    .storage
                    .registry(graph)?
                    .dropped_label_ids
                    .contains(&crate::graphid_label_id(endpoint))
            {
                return Err(Self::conflict("edge endpoint", endpoint));
            }
        }
        Ok(())
    }
    fn check_identity_stream(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<()> {
        let mut after = None;
        loop {
            let visible = self.ids(filter, after, 256)?;
            let current = self.write.storage.ids(filter, after, 256)?;
            if visible != current {
                return Err(Self::conflict(
                    "graph membership",
                    filter.graph.unwrap_or("global"),
                ));
            }
            if visible.is_empty() {
                return Ok(());
            }
            after = visible.last().copied();
        }
    }
    fn matches(&self, filter: GraphEntityFilter<'_>, id: u64) -> GraphStoreResult<bool> {
        if let Some(graph) = filter.graph {
            if !self.has_membership(filter.kind, id, graph)? {
                return Ok(false);
            }
        }
        Ok(match filter.kind {
            GraphEntityKind::Vertex => self
                .vertex(id)?
                .is_some_and(|row| filter.label.is_none_or(|label| row.label == label)),
            GraphEntityKind::Edge => self.edge(id)?.is_some_and(|row| {
                filter.label.is_none_or(|label| row.label == label)
                    && filter.source.is_none_or(|source| row.source_id == source)
                    && filter.target.is_none_or(|target| row.target_id == target)
            }),
        })
    }
}

struct OverlayCheckpoint {
    inner: Box<dyn GraphWriteTransaction>,
    state: Arc<Mutex<OverlayState>>,
    before: Changes,
    active: bool,
}
impl GraphWriteTransaction for OverlayCheckpoint {
    fn commit(&mut self) -> GraphStoreResult<()> {
        self.inner.commit()?;
        self.state.lock().write_depth -= 1;
        self.active = false;
        Ok(())
    }
    fn rollback(&mut self) -> GraphStoreResult<()> {
        if !self.active {
            return Ok(());
        }
        self.inner.rollback()?;
        let mut state = self.state.lock();
        state.changes.clone_from(&self.before);
        state.write_depth -= 1;
        self.active = false;
        Ok(())
    }
}
impl Drop for OverlayCheckpoint {
    fn drop(&mut self) {
        if self.active {
            let _ = self.rollback();
        }
    }
}

impl GraphStorage for OverlayGraphStorage {
    fn unmodified_read_snapshot(&self) -> Option<PersistentGraphStore> {
        let state = self.state.lock();
        (state.changes.vertices.is_empty()
            && state.changes.edges.is_empty()
            && state.changes.graphs.is_empty()
            && state.changes.registries.is_empty())
        .then(|| self.read.clone())
    }
    fn fork_overlay(&self) -> Option<Arc<dyn GraphStorage>> {
        let state = self.state.lock();
        Some(Arc::new(Self {
            write: self.write.clone(),
            read: self.read.clone(),
            state: Arc::new(Mutex::new(OverlayState {
                changes: state.changes.clone(),
                write_depth: 0,
            })),
        }))
    }
    fn begin_write(&self) -> GraphStoreResult<Box<dyn GraphWriteTransaction>> {
        let inner = self.write.storage.begin_write()?;
        let mut state = self.state.lock();
        let before = state.changes.clone();
        state.write_depth += 1;
        Ok(Box::new(OverlayCheckpoint {
            inner,
            state: Arc::clone(&self.state),
            before,
            active: true,
        }))
    }
    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        let changed = self.state.lock().changes.graphs.clone();
        let mut names: BTreeSet<_> = self.read.storage.graph_names()?.into_iter().collect();
        for name in changed {
            if self.write.storage.has_graph(&name)? {
                names.insert(name);
            } else {
                names.remove(&name);
            }
        }
        Ok(names.into_iter().collect())
    }
    fn has_graph(&self, graph: &str) -> GraphStoreResult<bool> {
        if self.state.lock().changes.graphs.contains(graph) {
            self.write.storage.has_graph(graph)
        } else {
            self.read.storage.has_graph(graph)
        }
    }
    fn create_graph(&self, graph: &str) -> GraphStoreResult<()> {
        if !self.state.lock().changes.graphs.contains(graph)
            && self.read.storage.has_graph(graph)? != self.write.storage.has_graph(graph)?
        {
            return Err(Self::conflict("graph", graph));
        }
        self.write.storage.create_graph(graph)?;
        self.state.lock().changes.graphs.insert(graph.to_owned());
        Ok(())
    }
    fn delete_graph(&self, graph: &str) -> GraphStoreResult<()> {
        for kind in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
            self.check_identity_stream(GraphEntityFilter::new(kind, Some(graph)))?;
        }
        self.write.storage.delete_graph(graph)?;
        self.state.lock().changes.graphs.insert(graph.to_owned());
        self.state
            .lock()
            .changes
            .registries
            .insert(graph.to_owned());
        Ok(())
    }
    fn registry(&self, graph: &str) -> GraphStoreResult<GraphLabelRegistry> {
        let state = self.state.lock();
        let live = state.write_depth > 0
            || state.changes.registries.contains(graph)
            || state.changes.graphs.contains(graph);
        drop(state);
        if live {
            self.write.storage.registry(graph)
        } else {
            self.read.storage.registry(graph)
        }
    }
    fn save_registry(&self, graph: &str, registry: &GraphLabelRegistry) -> GraphStoreResult<()> {
        let current = self.write.storage.registry(graph)?;
        for label in current.labels() {
            if registry.labels().iter().any(|next| {
                next.name == label.name && next.id == label.id && next.kind == label.kind
            }) {
                continue;
            }
            let kind = match label.kind {
                crate::LabelKind::Vertex => GraphEntityKind::Vertex,
                crate::LabelKind::Edge => GraphEntityKind::Edge,
            };
            let mut filter = GraphEntityFilter::new(kind, Some(graph));
            if label.id != label.kind.default_label_id() {
                filter.label = Some(&label.name);
            }
            self.check_identity_stream(filter)?;
        }
        self.write.storage.save_registry(graph, registry)?;
        self.state
            .lock()
            .changes
            .registries
            .insert(graph.to_owned());
        Ok(())
    }
    fn counter(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        self.write.storage.counter(kind)
    }
    fn save_counter(&self, kind: GraphEntityKind, next: u64) -> GraphStoreResult<()> {
        self.write.storage.save_counter(kind, next)
    }
    fn vertex(&self, id: u64) -> GraphStoreResult<Option<Vertex>> {
        if self.changed(GraphEntityKind::Vertex, id) {
            self.write.storage.vertex(id)
        } else {
            self.read.storage.vertex(id)
        }
    }
    fn edge(&self, id: u64) -> GraphStoreResult<Option<Edge>> {
        if self.changed(GraphEntityKind::Edge, id) {
            self.write.storage.edge(id)
        } else {
            self.read.storage.edge(id)
        }
    }
    fn save_vertex(&self, vertex: &Vertex) -> GraphStoreResult<()> {
        self.check_entity(GraphEntityKind::Vertex, vertex.vertex_id)?;
        self.write.storage.save_vertex(vertex)?;
        self.mark(GraphEntityKind::Vertex, vertex.vertex_id);
        Ok(())
    }
    fn save_edge(&self, edge: &Edge) -> GraphStoreResult<()> {
        self.check_entity(GraphEntityKind::Edge, edge.edge_id)?;
        for owner in self
            .write
            .storage
            .memberships(GraphEntityKind::Edge, edge.edge_id)?
        {
            self.check_edge_endpoints(edge, &owner)?;
        }
        self.write.storage.save_edge(edge)?;
        self.mark(GraphEntityKind::Edge, edge.edge_id);
        Ok(())
    }
    fn delete_vertex(&self, id: u64) -> GraphStoreResult<()> {
        self.check_entity(GraphEntityKind::Vertex, id)?;
        self.write.storage.delete_vertex(id)?;
        self.mark(GraphEntityKind::Vertex, id);
        Ok(())
    }
    fn delete_edge(&self, id: u64) -> GraphStoreResult<()> {
        self.check_entity(GraphEntityKind::Edge, id)?;
        self.write.storage.delete_edge(id)?;
        self.mark(GraphEntityKind::Edge, id);
        Ok(())
    }
    fn ids(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        filter.validate()?;
        if !(1..=uqa_storage::MAX_GRAPH_ID_PAGE).contains(&limit) {
            return Err(GraphStoreError::InvalidQuery(
                "invalid graph page size".into(),
            ));
        }
        let changed = self.state.lock().changes.ids(filter.kind).clone();
        let mut result = BTreeSet::new();
        let mut cursor = after;
        while result.len() < limit {
            let page = self.read.storage.ids(filter, cursor, limit)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().copied();
            for id in page {
                if !changed.contains(&id) && result.len() < limit {
                    result.insert(id);
                }
            }
        }
        for id in changed {
            if after.is_some_and(|after| id <= after) {
                continue;
            }
            if result.len() == limit && result.last().is_some_and(|last| id > *last) {
                break;
            }
            if self.matches(filter, id)? {
                result.insert(id);
                if result.len() > limit {
                    result.pop_last();
                }
            }
        }
        Ok(result.into_iter().collect())
    }
    fn count(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<u64> {
        let mut count = 0u64;
        let mut after = None;
        loop {
            let ids = self.ids(filter, after, 256)?;
            if ids.is_empty() {
                return Ok(count);
            }
            after = ids.last().copied();
            count = count
                .checked_add(
                    u64::try_from(ids.len())
                        .map_err(|error| GraphStoreError::InvalidQuery(error.to_string()))?,
                )
                .ok_or_else(|| GraphStoreError::InvalidQuery("graph count overflow".into()))?;
        }
    }
    fn max_id(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>> {
        self.write.storage.max_id(kind)
    }
    fn memberships(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<Vec<String>> {
        if self.changed(kind, id) {
            self.write.storage.memberships(kind, id)
        } else {
            self.read.storage.memberships(kind, id)
        }
    }
    fn has_membership(
        &self,
        kind: GraphEntityKind,
        id: u64,
        graph: &str,
    ) -> GraphStoreResult<bool> {
        if self.changed(kind, id) {
            self.write.storage.has_membership(kind, id, graph)
        } else {
            self.read.storage.has_membership(kind, id, graph)
        }
    }
    fn attach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.check_entity(kind, id)?;
        if kind == GraphEntityKind::Edge {
            if let Some(edge) = self.write.storage.edge(id)? {
                self.check_edge_endpoints(&edge, graph)?;
            }
        }
        self.write.storage.attach(kind, id, graph)?;
        self.mark(kind, id);
        Ok(())
    }
    fn detach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.check_entity(kind, id)?;
        if kind == GraphEntityKind::Vertex {
            self.check_vertex_adjacency(id, graph)?;
        }
        self.write.storage.detach(kind, id, graph)?;
        self.mark(kind, id);
        Ok(())
    }
}
