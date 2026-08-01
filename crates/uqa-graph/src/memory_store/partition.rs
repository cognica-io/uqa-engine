//! Per-graph membership and adjacency indexes.

use super::{BTreeMap, BTreeSet, Edge, EdgeId, Vertex, VertexId};

#[derive(Debug, Default, Clone)]
pub(super) struct Partition {
    pub(super) vertex_ids: BTreeSet<VertexId>,
    pub(super) edge_ids: BTreeSet<EdgeId>,
    pub(super) adj_out: BTreeMap<VertexId, BTreeSet<EdgeId>>,
    pub(super) adj_in: BTreeMap<VertexId, BTreeSet<EdgeId>>,
    pub(super) label_index: BTreeMap<String, BTreeSet<EdgeId>>,
    pub(super) vertex_label_index: BTreeMap<String, BTreeSet<VertexId>>,
}

impl Partition {
    pub(super) fn add_vertex(&mut self, vertex: &Vertex) {
        self.vertex_ids.insert(vertex.vertex_id);
        self.vertex_label_index
            .entry(vertex.label.clone())
            .or_default()
            .insert(vertex.vertex_id);
    }

    pub(super) fn add_edge(&mut self, edge: &Edge) {
        self.edge_ids.insert(edge.edge_id);
        self.adj_out
            .entry(edge.source_id)
            .or_default()
            .insert(edge.edge_id);
        self.adj_in
            .entry(edge.target_id)
            .or_default()
            .insert(edge.edge_id);
        self.label_index
            .entry(edge.label.clone())
            .or_default()
            .insert(edge.edge_id);
    }

    pub(super) fn remove_edge(&mut self, edge: &Edge) {
        self.edge_ids.remove(&edge.edge_id);
        if let Some(set) = self.adj_out.get_mut(&edge.source_id) {
            set.remove(&edge.edge_id);
        }
        if let Some(set) = self.adj_in.get_mut(&edge.target_id) {
            set.remove(&edge.edge_id);
        }
        if let Some(set) = self.label_index.get_mut(&edge.label) {
            set.remove(&edge.edge_id);
        }
    }
}
