//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph operations over indexed durable records and storage checkpoints.

use super::{
    BTreeMap, BTreeSet, Direction, Edge, GraphEntityFilter, GraphEntityKind, GraphLabelRegistry,
    GraphStore, GraphStoreError, GraphStoreResult, LabelKind, PersistentGraphStore, Vertex,
};

impl GraphStore for PersistentGraphStore {
    fn vertex_id_page(
        &self,
        graph: &str,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        self.require_graph(graph)?;
        self.storage.ids(
            GraphEntityFilter::new(GraphEntityKind::Vertex, Some(graph)),
            after,
            limit,
        )
    }
    fn edge_id_page(
        &self,
        graph: &str,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>> {
        self.require_graph(graph)?;
        self.storage.ids(
            GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph)),
            after,
            limit,
        )
    }
    fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> GraphStoreResult<T>,
    ) -> GraphStoreResult<T> {
        self.transaction_mapped(operation, std::convert::identity)
    }

    fn create_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        self.transaction(|store| {
            if !store.storage.has_graph(name)? {
                store.storage.create_graph(name)?;
                store
                    .storage
                    .save_registry(name, &GraphLabelRegistry::default())?;
            }
            Ok(())
        })
    }
    fn drop_graph(&mut self, name: &str) -> GraphStoreResult<()> {
        self.transaction(|store| {
            if !store.storage.has_graph(name)? {
                return Ok(());
            }
            for kind in [GraphEntityKind::Edge, GraphEntityKind::Vertex] {
                store.for_each_id(GraphEntityFilter::new(kind, Some(name)), |id| {
                    store.detach_entity(kind, id, name)
                })?;
            }
            store.storage.delete_graph(name)
        })
    }
    fn graph_names(&self) -> GraphStoreResult<Vec<String>> {
        self.storage.graph_names()
    }
    fn has_graph(&self, name: &str) -> GraphStoreResult<bool> {
        self.storage.has_graph(name)
    }
    fn union_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        self.algebra(g1, Some(g2), target, |_| true, true)
    }
    fn intersect_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        self.algebra(g1, Some(g2), target, |common| common, false)
    }
    fn difference_graphs(&mut self, g1: &str, g2: &str, target: &str) -> GraphStoreResult<()> {
        self.algebra(g1, Some(g2), target, |common| !common, false)
    }
    fn copy_graph(&mut self, source: &str, target: &str) -> GraphStoreResult<()> {
        self.algebra(source, None, target, |_| true, false)
    }

    fn add_vertex(&mut self, vertex: Vertex, graph: &str) -> GraphStoreResult<()> {
        self.transaction(|store| {
            store.require_graph(graph)?;
            store.reserve_id(GraphEntityKind::Vertex, vertex.vertex_id)?;
            store.storage.save_vertex(&vertex)?;
            store
                .storage
                .attach(GraphEntityKind::Vertex, vertex.vertex_id, graph)?;
            for owner in store
                .storage
                .memberships(GraphEntityKind::Vertex, vertex.vertex_id)?
            {
                let mut registry = store.label_registry(&owner)?;
                registry.observe(&vertex.label, vertex.vertex_id, LabelKind::Vertex);
                store.storage.save_registry(&owner, &registry)?;
            }
            Ok(())
        })
    }
    fn add_edge(&mut self, edge: Edge, graph: &str) -> GraphStoreResult<()> {
        self.transaction(|store| {
            store.require_graph(graph)?;
            let mut owners = store
                .storage
                .memberships(GraphEntityKind::Edge, edge.edge_id)?;
            if !owners.iter().any(|owner| owner == graph) {
                owners.push(graph.to_owned());
            }
            for owner in &owners {
                for id in [edge.source_id, edge.target_id] {
                    if !store
                        .storage
                        .has_membership(GraphEntityKind::Vertex, id, owner)?
                    {
                        return Err(GraphStoreError::InvalidMutation(format!(
                            "edge {} references endpoint outside graph {owner:?}: {} -> {}",
                            edge.edge_id, edge.source_id, edge.target_id
                        )));
                    }
                    store.require_vertex(id)?;
                }
            }
            store.reserve_id(GraphEntityKind::Edge, edge.edge_id)?;
            store.storage.save_edge(&edge)?;
            store
                .storage
                .attach(GraphEntityKind::Edge, edge.edge_id, graph)?;
            for owner in owners {
                let mut registry = store.label_registry(&owner)?;
                registry.observe(&edge.label, edge.edge_id, LabelKind::Edge);
                store.storage.save_registry(&owner, &registry)?;
            }
            Ok(())
        })
    }
    fn remove_vertex(&mut self, vertex_id: u64, graph: &str) -> GraphStoreResult<()> {
        self.transaction(|store| {
            store.require_graph(graph)?;
            if !store
                .storage
                .has_membership(GraphEntityKind::Vertex, vertex_id, graph)?
            {
                return Ok(());
            }
            for outgoing in [true, false] {
                let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
                if outgoing {
                    filter.source = Some(vertex_id);
                } else {
                    filter.target = Some(vertex_id);
                }
                store.for_each_id(filter, |id| {
                    store.detach_entity(GraphEntityKind::Edge, id, graph)
                })?;
            }
            store.detach_entity(GraphEntityKind::Vertex, vertex_id, graph)
        })
    }
    fn remove_edge(&mut self, edge_id: u64, graph: &str) -> GraphStoreResult<()> {
        self.transaction(|store| {
            store.require_graph(graph)?;
            store.detach_entity(GraphEntityKind::Edge, edge_id, graph)
        })
    }

    fn neighbors(
        &self,
        vertex_id: u64,
        label: Option<&str>,
        direction: Direction,
        graph: &str,
    ) -> GraphStoreResult<Vec<u64>> {
        self.require_vertex_in_graph(vertex_id, graph)?;
        let mut neighbors = Vec::new();
        for outgoing in [true, false] {
            if (outgoing && direction == Direction::In)
                || (!outgoing && direction == Direction::Out)
            {
                continue;
            }
            let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
            filter.label = label;
            if outgoing {
                filter.source = Some(vertex_id);
            } else {
                filter.target = Some(vertex_id);
            }
            self.for_each_id(filter, |id| {
                let edge = self.require_edge(id, graph)?;
                neighbors.push(if outgoing {
                    edge.target_id
                } else {
                    edge.source_id
                });
                Ok(())
            })?;
        }
        if direction == Direction::Both {
            neighbors.sort_unstable();
            neighbors.dedup();
        }
        Ok(neighbors)
    }
    fn vertices_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Vertex>> {
        self.require_graph(graph)?;
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some(graph));
        filter.label = Some(label);
        let mut result = Vec::new();
        self.for_each_id(filter, |id| {
            result.push(self.require_vertex(id)?);
            Ok(())
        })?;
        Ok(result)
    }
    fn vertex_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<u64>> {
        self.require_graph(graph)?;
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Vertex, Some(graph));
        filter.label = Some(label);
        Ok(self.collect_ids(filter)?.into_iter().collect())
    }
    fn vertices_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Vertex>> {
        self.require_graph(graph)?;
        let mut result = Vec::new();
        self.for_each_id(
            GraphEntityFilter::new(GraphEntityKind::Vertex, Some(graph)),
            |id| {
                result.push(self.require_vertex(id)?);
                Ok(())
            },
        )?;
        Ok(result)
    }
    fn edges_in_graph(&self, graph: &str) -> GraphStoreResult<Vec<Edge>> {
        self.require_graph(graph)?;
        let mut result = Vec::new();
        self.for_each_id(
            GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph)),
            |id| {
                result.push(self.require_edge(id, graph)?);
                Ok(())
            },
        )?;
        Ok(result)
    }
    fn vertex_graphs(&self, id: u64) -> GraphStoreResult<BTreeSet<String>> {
        Ok(self
            .storage
            .memberships(GraphEntityKind::Vertex, id)?
            .into_iter()
            .collect())
    }
    fn edges_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<Vec<Edge>> {
        self.require_graph(graph)?;
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
        filter.label = Some(label);
        let mut edges = Vec::new();
        self.for_each_id(filter, |id| {
            edges.push(self.require_edge(id, graph)?);
            Ok(())
        })?;
        Ok(edges)
    }

    fn edge_graphs(&self, id: u64) -> GraphStoreResult<BTreeSet<String>> {
        Ok(self
            .storage
            .memberships(GraphEntityKind::Edge, id)?
            .into_iter()
            .collect())
    }
    fn out_edge_ids(&self, id: u64, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        self.require_vertex_in_graph(id, graph)?;
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
        filter.source = Some(id);
        let ids = self.collect_ids(filter)?;
        for id in &ids {
            self.require_edge(*id, graph)?;
        }
        Ok(ids)
    }
    fn in_edge_ids(&self, id: u64, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        self.require_vertex_in_graph(id, graph)?;
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
        filter.target = Some(id);
        let ids = self.collect_ids(filter)?;
        for id in &ids {
            self.require_edge(*id, graph)?;
        }
        Ok(ids)
    }
    fn edge_ids_by_label(&self, label: &str, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        self.require_graph(graph)?;
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
        filter.label = Some(label);
        let ids = self.collect_ids(filter)?;
        for id in &ids {
            self.require_edge(*id, graph)?;
        }
        Ok(ids)
    }
    fn vertex_ids_in_graph(&self, graph: &str) -> GraphStoreResult<BTreeSet<u64>> {
        self.require_graph(graph)?;
        self.collect_ids(GraphEntityFilter::new(GraphEntityKind::Vertex, Some(graph)))
    }
    fn require_vertex_in_graph(&self, id: u64, graph: &str) -> GraphStoreResult<()> {
        self.require_graph(graph)?;
        if !self
            .storage
            .has_membership(GraphEntityKind::Vertex, id, graph)?
        {
            return Err(GraphStoreError::InvalidQuery(format!(
                "vertex {id} is not a member of graph {graph:?}"
            )));
        }
        self.require_vertex(id)?;
        Ok(())
    }

    fn degree_distribution(&self, graph: &str) -> GraphStoreResult<BTreeMap<u64, u64>> {
        self.require_graph(graph)?;
        let mut result = BTreeMap::new();
        self.for_each_id(
            GraphEntityFilter::new(GraphEntityKind::Vertex, Some(graph)),
            |id| {
                let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
                filter.source = Some(id);
                result.insert(id, self.storage.count(filter)?);
                Ok(())
            },
        )?;
        Ok(result)
    }
    fn label_degree(&self, label: &str, graph: &str) -> GraphStoreResult<f64> {
        self.require_graph(graph)?;
        let mut sources = BTreeSet::new();
        let mut count = 0_usize;
        let mut filter = GraphEntityFilter::new(GraphEntityKind::Edge, Some(graph));
        filter.label = Some(label);
        self.for_each_id(filter, |id| {
            sources.insert(self.require_edge(id, graph)?.source_id);
            count += 1;
            Ok(())
        })?;
        if sources.is_empty() {
            return Ok(0.0);
        }
        Ok(
            crate::memory_store::usize_to_f64_exact(count, "edge label count")?
                / crate::memory_store::usize_to_f64_exact(
                    sources.len(),
                    "edge label source count",
                )?,
        )
    }
    fn vertex_label_counts(&self, graph: &str) -> GraphStoreResult<BTreeMap<String, u64>> {
        self.require_graph(graph)?;
        let mut result = BTreeMap::new();
        self.for_each_id(
            GraphEntityFilter::new(GraphEntityKind::Vertex, Some(graph)),
            |id| {
                *result.entry(self.require_vertex(id)?.label).or_default() += 1;
                Ok(())
            },
        )?;
        Ok(result)
    }
    fn get_vertex(&self, id: u64) -> GraphStoreResult<Option<Vertex>> {
        self.storage.vertex(id)
    }
    fn get_edge(&self, id: u64) -> GraphStoreResult<Option<Edge>> {
        self.storage.edge(id)
    }
    fn next_vertex_id(&mut self) -> GraphStoreResult<u64> {
        self.allocate_counter(GraphEntityKind::Vertex)
    }
    fn next_edge_id(&mut self) -> GraphStoreResult<u64> {
        self.allocate_counter(GraphEntityKind::Edge)
    }
    fn allocate_vertex_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<u64> {
        self.allocate_label_id(label, graph, LabelKind::Vertex)
    }
    fn allocate_edge_id(&mut self, label: &str, graph: &str) -> GraphStoreResult<u64> {
        self.allocate_label_id(label, graph, LabelKind::Edge)
    }
    fn clear(&mut self) -> GraphStoreResult<()> {
        self.transaction(|store| {
            for graph in store.graph_names()? {
                store.drop_graph(&graph)?;
            }
            for kind in [GraphEntityKind::Vertex, GraphEntityKind::Edge] {
                // Also release global records left without any membership.
                store.for_each_id(GraphEntityFilter::new(kind, None), |id| match kind {
                    GraphEntityKind::Vertex => store.storage.delete_vertex(id),
                    GraphEntityKind::Edge => store.storage.delete_edge(id),
                })?;
                store.storage.save_counter(kind, 1)?;
            }
            Ok(())
        })
    }
    fn vertices(&self) -> GraphStoreResult<BTreeMap<u64, Vertex>> {
        let mut result = BTreeMap::new();
        self.for_each_id(
            GraphEntityFilter::new(GraphEntityKind::Vertex, None),
            |id| {
                result.insert(id, self.require_vertex(id)?);
                Ok(())
            },
        )?;
        Ok(result)
    }
    fn edges(&self) -> GraphStoreResult<BTreeMap<u64, Edge>> {
        let mut result = BTreeMap::new();
        self.for_each_id(GraphEntityFilter::new(GraphEntityKind::Edge, None), |id| {
            let edge = self
                .storage
                .edge(id)?
                .ok_or_else(|| GraphStoreError::CorruptGraph(format!("missing edge {id}")))?;
            result.insert(id, edge);
            Ok(())
        })?;
        Ok(result)
    }
}
