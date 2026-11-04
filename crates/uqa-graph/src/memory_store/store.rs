//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory state management and persistence hydration helpers.

use super::{
    graphid_label_id, BTreeSet, Edge, EdgeId, GraphLabelInfo, GraphLabelRegistry, GraphStore as _,
    GraphStoreError, GraphStoreResult, LabelKind, MemoryGraphStore, Partition, Vertex, VertexId,
};

impl MemoryGraphStore {
    pub fn new() -> Self {
        Self {
            next_vertex_id: 1,
            next_edge_id: 1,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn remove_edge_record_for_corruption_test(&mut self, edge_id: EdgeId) {
        self.edges.remove(&edge_id).unwrap();
    }

    #[cfg(test)]
    pub(super) fn remove_vertex_record_for_corruption_test(&mut self, vertex_id: VertexId) {
        self.vertices.remove(&vertex_id).unwrap();
    }

    pub(super) fn partition_candidate(&self, name: &str) -> GraphStoreResult<Partition> {
        self.require_partition(name).cloned()
    }

    pub(super) fn require_partition(&self, name: &str) -> GraphStoreResult<&Partition> {
        self.graphs
            .get(name)
            .ok_or_else(|| GraphStoreError::UnknownGraph(name.to_string()))
    }

    pub(super) fn require_query_vertex(
        &self,
        partition: &Partition,
        vertex_id: VertexId,
        graph: &str,
    ) -> GraphStoreResult<()> {
        if !partition.vertex_ids.contains(&vertex_id) {
            return Err(GraphStoreError::InvalidQuery(format!(
                "vertex {vertex_id} is not a member of graph {graph:?}"
            )));
        }
        self.require_partition_vertex(partition, vertex_id, graph)
            .map(|_| ())
    }

    pub(super) fn require_partition_vertex<'a>(
        &'a self,
        partition: &Partition,
        vertex_id: VertexId,
        graph: &str,
    ) -> GraphStoreResult<&'a Vertex> {
        if !partition.vertex_ids.contains(&vertex_id) {
            return Err(GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} references vertex {vertex_id} outside its membership set"
            )));
        }
        self.vertices.get(&vertex_id).ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} references missing vertex {vertex_id}"
            ))
        })
    }

    pub(super) fn require_partition_edge<'a>(
        &'a self,
        partition: &Partition,
        edge_id: EdgeId,
        graph: &str,
    ) -> GraphStoreResult<&'a Edge> {
        if !partition.edge_ids.contains(&edge_id) {
            return Err(GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} adjacency references edge {edge_id} outside its membership set"
            )));
        }
        let edge = self.edges.get(&edge_id).ok_or_else(|| {
            GraphStoreError::CorruptGraph(format!(
                "graph {graph:?} references missing edge {edge_id}"
            ))
        })?;
        self.require_edge_endpoint(partition, edge.source_id, graph)?;
        self.require_edge_endpoint(partition, edge.target_id, graph)?;
        Ok(edge)
    }

    pub(super) fn require_edge_endpoint(
        &self,
        partition: &Partition,
        vertex_id: VertexId,
        graph: &str,
    ) -> GraphStoreResult<()> {
        if partition.vertex_ids.contains(&vertex_id) && self.vertices.contains_key(&vertex_id) {
            return Ok(());
        }
        let label_id = graphid_label_id(vertex_id);
        if self
            .native_registry(graph)
            .dropped_label_ids
            .contains(&label_id)
        {
            return Ok(());
        }
        Err(GraphStoreError::CorruptGraph(format!(
            "graph {graph:?} edge references missing vertex {vertex_id}"
        )))
    }

    pub(super) fn create_partition(&mut self, name: &str) -> GraphStoreResult<()> {
        if !self.graphs.contains_key(name) {
            self.put_partition(name, Partition::new(self.graphs.budget()))?;
        }
        Ok(())
    }

    pub(super) fn release_vertex_if_orphan(&mut self, id: VertexId) -> GraphStoreResult<()> {
        if self
            .vertex_membership
            .get(&id)
            .is_none_or(super::NativeSet::is_empty)
        {
            self.vertices.remove(&id)?;
            self.vertex_membership.remove(&id)?;
        }
        Ok(())
    }

    pub(super) fn release_edge_if_orphan(&mut self, id: EdgeId) -> GraphStoreResult<()> {
        if self
            .edge_membership
            .get(&id)
            .is_none_or(super::NativeSet::is_empty)
        {
            self.edges.remove(&id)?;
            self.edge_membership.remove(&id)?;
        }
        Ok(())
    }

    pub(super) fn populate_graph_from_ids(
        &mut self,
        vertex_ids: &BTreeSet<VertexId>,
        edge_ids: &BTreeSet<EdgeId>,
        target: &str,
    ) -> GraphStoreResult<()> {
        for id in vertex_ids {
            if !self.vertices.contains_key(id) {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "graph membership references missing vertex {id}"
                )));
            }
        }
        for id in edge_ids {
            if !self.edges.contains_key(id) {
                return Err(GraphStoreError::CorruptGraph(format!(
                    "graph membership references missing edge {id}"
                )));
            }
        }
        self.publish(|state| {
            let mut partition = state
                .graphs
                .get(target)
                .cloned()
                .unwrap_or_else(|| Partition::new(state.graphs.budget()));
            for id in vertex_ids {
                partition.add_vertex(state.vertices.get(id).expect("validated vertex"))?;
                super::publication::membership(&mut state.vertex_membership, *id, target, true)?;
            }
            for id in edge_ids {
                partition.add_edge(state.edges.get(id).expect("validated edge"))?;
                super::publication::membership(&mut state.edge_membership, *id, target, true)?;
            }
            state.put_partition(target, partition)
        })
    }

    /// Insert a vertex into the global registry without attaching it
    /// to any graph. Used by the `SQLite`-backed store on hydration
    /// before membership is restored from the persisted catalog.
    pub fn insert_raw_vertex(&mut self, vertex: Vertex) -> GraphStoreResult<()> {
        self.publish(|state| state.insert_raw_vertex_candidate(vertex))
    }

    fn insert_raw_vertex_candidate(&mut self, vertex: Vertex) -> GraphStoreResult<()> {
        let next = if vertex.vertex_id >= self.next_vertex_id {
            vertex.vertex_id.checked_add(1).ok_or_else(|| {
                GraphStoreError::IdExhausted("raw vertex id counter overflow".into())
            })?
        } else {
            self.next_vertex_id
        };
        let bytes = super::publication::entity_bytes(
            &vertex.label,
            &vertex.properties,
            self.vertices.budget(),
        )?;
        self.vertices
            .insert(vertex.vertex_id, vertex, bytes, bytes)?;
        self.next_vertex_id = next;
        Ok(())
    }

    /// Insert an edge before graph memberships are restored. Endpoint
    /// validation is deferred until `attach_edge`, when the graph's persisted
    /// label tombstones are available.
    pub fn insert_raw_edge(&mut self, edge: Edge) -> GraphStoreResult<()> {
        self.publish(|state| state.insert_raw_edge_candidate(edge))
    }

    fn insert_raw_edge_candidate(&mut self, edge: Edge) -> GraphStoreResult<()> {
        let next = if edge.edge_id >= self.next_edge_id {
            edge.edge_id.checked_add(1).ok_or_else(|| {
                GraphStoreError::IdExhausted("raw edge id counter overflow".into())
            })?
        } else {
            self.next_edge_id
        };
        let bytes =
            super::publication::entity_bytes(&edge.label, &edge.properties, self.edges.budget())?;
        self.edges.insert(edge.edge_id, edge, bytes, bytes)?;
        self.next_edge_id = next;
        Ok(())
    }

    /// Attach a previously inserted vertex to the named graph. The
    /// graph must already exist (`create_graph`).
    pub fn attach_vertex(&mut self, vertex_id: VertexId, graph: &str) -> GraphStoreResult<()> {
        self.publish(|state| {
            let vertex = state.vertices.get(&vertex_id).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!("cannot attach missing vertex {vertex_id}"))
            })?;
            let mut partition = state.partition_candidate(graph)?;
            partition.add_vertex(vertex)?;
            super::publication::membership(&mut state.vertex_membership, vertex_id, graph, true)?;
            state.put_partition(graph, partition)
        })
    }

    pub fn attach_edge(&mut self, edge_id: EdgeId, graph: &str) -> GraphStoreResult<()> {
        self.publish(|state| {
            let edge = state.edges.get(&edge_id).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!("cannot attach missing edge {edge_id}"))
            })?;
            let mut partition = state.partition_candidate(graph)?;
            state.require_edge_endpoint(&partition, edge.source_id, graph)?;
            state.require_edge_endpoint(&partition, edge.target_id, graph)?;
            partition.add_edge(edge)?;
            super::publication::membership(&mut state.edge_membership, edge_id, graph, true)?;
            state.put_partition(graph, partition)
        })
    }

    /// All edge ids that participate in `graph`, in stable id order.
    /// Helper for the `SQLite`-backed store's bulk write paths.
    pub fn out_edge_ids_for_graph(&self, graph: &str) -> GraphStoreResult<BTreeSet<EdgeId>> {
        Ok(self.require_partition(graph)?.edge_ids.to_set())
    }

    fn remove_vertex_preserving_incident_edges(
        &mut self,
        vertex_id: VertexId,
        graph: &str,
    ) -> GraphStoreResult<()> {
        let mut partition = self.partition_candidate(graph)?;
        if !partition.vertex_ids.contains(&vertex_id) {
            return Ok(());
        }
        let vertex = self.require_partition_vertex(&partition, vertex_id, graph)?;
        partition.remove_vertex(vertex)?;
        self.put_partition(graph, partition)?;
        super::publication::membership(&mut self.vertex_membership, vertex_id, graph, false)?;
        self.release_vertex_if_orphan(vertex_id)
    }

    /// Snapshot of the AGE label registry for `graph` (empty registry
    /// when the graph has never allocated an id).
    pub fn label_registry(&self, graph: &str) -> GraphLabelRegistry {
        self.native_registry(graph).to_record()
    }

    pub(super) fn native_registry(&self, graph: &str) -> super::NativeLabels {
        self.label_registries
            .get(graph)
            .cloned()
            .unwrap_or_else(|| super::NativeLabels::new(self.label_registries.budget()))
    }

    /// Install (merge) a persisted label registry for `graph`. Existing
    /// in-memory state wins on conflicts except that the larger
    /// sequence / label-id watermarks are kept.
    pub fn import_label_registry(
        &mut self,
        graph: &str,
        registry: &GraphLabelRegistry,
    ) -> GraphStoreResult<()> {
        let mut candidate = self.native_registry(graph);
        candidate.merge(registry)?;
        self.put_registry(graph, candidate)
    }

    /// Re-derive the label registry of `graph` from the ids of the
    /// entities currently attached to it. Self-heals restored graphs
    /// whose registry metadata is missing: `label_id = id >> 48`.
    pub fn rebuild_label_registry_from_ids(&mut self, graph: &str) -> GraphStoreResult<()> {
        let mut registry = self.native_registry(graph);
        if let Some(part) = self.graphs.get(graph) {
            for id in &part.vertex_ids {
                if let Some(vertex) = self.vertices.get(id) {
                    registry.observe(&vertex.label, *id, LabelKind::Vertex)?;
                }
            }
            for id in &part.edge_ids {
                if let Some(edge) = self.edges.get(id) {
                    registry.observe(&edge.label, *id, LabelKind::Edge)?;
                }
            }
        }
        self.put_registry(graph, registry)
    }

    /// Every present `ag_label` entry of `graph`, in label-id order.
    pub fn graph_labels(&self, graph: &str) -> GraphStoreResult<Vec<GraphLabelInfo>> {
        self.require_partition(graph)?;
        Ok(self.native_registry(graph).labels())
    }

    /// The kind of `label` in `graph`, or `None` when the label is not
    /// registered there.
    pub fn graph_label_kind(
        &self,
        graph: &str,
        label: &str,
    ) -> GraphStoreResult<Option<LabelKind>> {
        self.require_partition(graph)?;
        Ok(self.native_registry(graph).label_kind(label))
    }

    /// Register an empty user label in `graph` (`create_vlabel` /
    /// `create_elabel`). Returns the label id, or `None` when a label of
    /// that name already exists in the graph. A missing default relation of
    /// the requested kind prevents new child labels, as it does in AGE.
    pub fn create_label(
        &mut self,
        graph: &str,
        label: &str,
        kind: LabelKind,
    ) -> GraphStoreResult<Option<u32>> {
        self.require_partition(graph)?;
        let mut candidate = self.native_registry(graph);
        let id = candidate.register_label(label, kind)?;
        if id.is_some() {
            self.put_registry(graph, candidate)?;
        }
        Ok(id)
    }

    /// Drop a label from `graph` together with every entity that carries it
    /// (`drop_label`, which AGE implements as `DROP TABLE` on the label
    /// relation). Vertex rows are removed while incident rows in edge-label
    /// relations survive with dangling endpoint ids. A default label can be
    /// dropped only when no user label of that kind inherits from it. Returns
    /// the released label id and kind, or `None` when the label is not
    /// registered.
    pub fn drop_label(
        &mut self,
        graph: &str,
        label: &str,
    ) -> GraphStoreResult<Option<(u32, LabelKind)>> {
        self.publish(|state| state.drop_label_candidate(graph, label))
    }

    fn drop_label_candidate(
        &mut self,
        graph: &str,
        label: &str,
    ) -> GraphStoreResult<Option<(u32, LabelKind)>> {
        self.require_partition(graph)?;
        let registry = self.native_registry(graph);
        let Some(kind) = registry.label_kind(label) else {
            return Ok(None);
        };
        let id = if label == kind.default_label_name() {
            if let Some(dependent) = registry
                .labels
                .keys()
                .find(|candidate| registry.label_kind(candidate) == Some(kind))
            {
                return Err(GraphStoreError::InvalidMutation(format!(
                    "cannot drop default label {label} while label {dependent} depends on it"
                )));
            }
            kind.default_label_id()
        } else {
            registry.labels.get(label).copied().ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "graph {graph:?} label {label:?} has no registry id"
                ))
            })?
        };
        match kind {
            LabelKind::Vertex => {
                let vertex_ids = if id == kind.default_label_id() {
                    self.require_partition(graph)?
                        .vertex_ids
                        .iter()
                        .copied()
                        .filter(|vertex_id| graphid_label_id(*vertex_id) == id)
                        .collect()
                } else {
                    self.vertex_ids_by_label(label, graph)?
                };
                for vertex_id in vertex_ids {
                    self.remove_vertex_preserving_incident_edges(vertex_id, graph)?;
                }
            }
            LabelKind::Edge => {
                let edge_ids = if id == kind.default_label_id() {
                    self.require_partition(graph)?
                        .edge_ids
                        .iter()
                        .copied()
                        .filter(|edge_id| graphid_label_id(*edge_id) == id)
                        .collect()
                } else {
                    self.edge_ids_by_label(label, graph)?
                };
                for edge_id in edge_ids {
                    self.remove_edge(edge_id, graph)?;
                }
            }
        }
        let mut registry = self.native_registry(graph);
        registry.remove_label(label)?;
        self.put_registry(graph, registry)?;
        Ok(Some((id, kind)))
    }

    /// Rename graph `from` to `to`, carrying its partition, memberships,
    /// and label registry over. Fails when `from` is missing or `to`
    /// already exists.
    pub fn rename_graph(&mut self, from: &str, to: &str) -> GraphStoreResult<()> {
        if from == to {
            return Ok(());
        }
        if self.graphs.contains_key(to) {
            return Err(GraphStoreError::InvalidMutation(format!(
                "graph {to:?} already exists"
            )));
        }
        let partition = self.partition_candidate(from)?;
        self.publish(|state| {
            for id in &partition.vertex_ids {
                super::publication::membership(&mut state.vertex_membership, *id, from, false)?;
                super::publication::membership(&mut state.vertex_membership, *id, to, true)?;
            }
            for id in &partition.edge_ids {
                super::publication::membership(&mut state.edge_membership, *id, from, false)?;
                super::publication::membership(&mut state.edge_membership, *id, to, true)?;
            }
            state.graphs.remove(from)?;
            state.put_partition(to, partition)?;
            if let Some(registry) = state.label_registries.get(from).cloned() {
                state.put_registry(to, registry)?;
                state.label_registries.remove(from)?;
            }
            Ok(())
        })
    }
}
