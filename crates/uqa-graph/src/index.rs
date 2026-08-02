//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Specialized graph indexes for accelerating traversal and RPQ
//! evaluation (Section 6.4, Paper 2).
//!
//! `LabelIndex` exposes label cardinality and label-to-vertex sets on
//! top of what the underlying [`GraphStore`] already tracks.
//! `PathIndex` pre-computes the `(start, end)` reachability set for a
//! list of label sequences so the RPQ operator can short-circuit when
//! the input expression is a pure label-concatenation.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{EdgeId, Value, VertexId};

use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};

#[derive(Debug, Clone, Default)]
pub struct LabelIndex {
    label_to_edges: BTreeMap<String, Vec<EdgeId>>,
    label_to_vertices: BTreeMap<String, BTreeSet<VertexId>>,
}

impl LabelIndex {
    pub fn build<G: GraphStore>(store: &G, graph: &str) -> GraphStoreResult<Self> {
        let mut idx = Self::default();
        for vid in store.vertex_ids_in_graph(graph)? {
            for eid in store.out_edge_ids(vid, graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing indexed edge {eid}"))
                })?;
                idx.label_to_edges
                    .entry(edge.label.clone())
                    .or_default()
                    .push(eid);
                let vset = idx.label_to_vertices.entry(edge.label.clone()).or_default();
                vset.insert(edge.source_id);
                vset.insert(edge.target_id);
            }
        }
        for edges in idx.label_to_edges.values_mut() {
            edges.sort_unstable();
            edges.dedup();
        }
        Ok(idx)
    }

    pub fn edges_by_label(&self, label: &str) -> &[EdgeId] {
        self.label_to_edges.get(label).map_or(&[], Vec::as_slice)
    }

    pub fn vertices_by_label(&self, label: &str) -> Option<&BTreeSet<VertexId>> {
        self.label_to_vertices.get(label)
    }

    pub fn labels(&self) -> Vec<String> {
        self.label_to_edges.keys().cloned().collect()
    }

    pub fn label_count(&self, label: &str) -> usize {
        self.label_to_edges.get(label).map_or(0, Vec::len)
    }
}

/// Immutable equality index over selected vertex properties in one named graph.
///
/// Missing properties are not indexed, while an explicitly stored
/// [`Value::Null`] remains queryable. Value equality follows [`Value`]'s total
/// ordering, including its cross-numeric equality rules, so indexed lookup and
/// relational equality agree.
#[derive(Debug, Clone, Default)]
pub struct VertexPropertyIndex {
    property_values: BTreeMap<String, BTreeMap<Value, BTreeSet<VertexId>>>,
}

impl VertexPropertyIndex {
    pub fn build<G: GraphStore>(
        store: &G,
        graph: &str,
        properties: &[&str],
    ) -> GraphStoreResult<Self> {
        let property_names: BTreeSet<String> = properties
            .iter()
            .map(|property| (*property).to_owned())
            .collect();
        let mut property_values: BTreeMap<String, BTreeMap<Value, BTreeSet<VertexId>>> =
            property_names
                .iter()
                .map(|property| (property.clone(), BTreeMap::new()))
                .collect();

        for vertex_id in store.vertex_ids_in_graph(graph)? {
            let vertex = store.get_vertex(vertex_id).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!(
                    "graph {graph:?} references missing indexed vertex {vertex_id}"
                ))
            })?;
            for property in &property_names {
                let Some(value) = vertex.properties.get(property) else {
                    continue;
                };
                property_values
                    .get_mut(property)
                    .expect("requested property was initialized")
                    .entry(value.clone())
                    .or_default()
                    .insert(vertex_id);
            }
        }

        Ok(Self { property_values })
    }

    pub fn has_property(&self, property: &str) -> bool {
        self.property_values.contains_key(property)
    }

    pub fn lookup_eq(&self, property: &str, value: &Value) -> Option<&BTreeSet<VertexId>> {
        self.property_values.get(property)?.get(value)
    }

    pub fn property_names(&self) -> impl Iterator<Item = &str> {
        self.property_values.keys().map(String::as_str)
    }
}

/// Pre-indexed reachable `(start, end)` pairs for fixed label
/// sequences. Lookup is keyed by the slash-joined sequence so the RPQ
/// operator can lift a `Label / Label / ...` expression into a direct
/// hit without running NFA simulation.
#[derive(Debug, Clone, Default)]
pub struct PathIndex {
    path_pairs: BTreeMap<String, BTreeSet<(VertexId, VertexId)>>,
}

impl PathIndex {
    pub fn build<G: GraphStore>(
        store: &G,
        graph: &str,
        label_sequences: &[Vec<String>],
    ) -> GraphStoreResult<Self> {
        let mut idx = Self::default();
        for seq in label_sequences {
            let key = seq.join("/");
            let mut pairs: BTreeSet<(VertexId, VertexId)> = BTreeSet::new();
            for start in store.vertex_ids_in_graph(graph)? {
                let ends = follow_path(store, graph, start, seq)?;
                for end in ends {
                    pairs.insert((start, end));
                }
            }
            idx.path_pairs.insert(key, pairs);
        }
        Ok(idx)
    }

    pub fn lookup(&self, label_sequence: &[String]) -> Option<&BTreeSet<(VertexId, VertexId)>> {
        let key = label_sequence.join("/");
        self.path_pairs.get(&key)
    }

    pub fn has_path(&self, label_sequence: &[String]) -> bool {
        let key = label_sequence.join("/");
        self.path_pairs.contains_key(&key)
    }

    pub fn indexed_paths(&self) -> Vec<String> {
        self.path_pairs.keys().cloned().collect()
    }
}

fn follow_path<G: GraphStore>(
    store: &G,
    graph: &str,
    start: VertexId,
    labels: &[String],
) -> GraphStoreResult<BTreeSet<VertexId>> {
    let mut current: BTreeSet<VertexId> = BTreeSet::from([start]);
    for label in labels {
        let mut next_set: BTreeSet<VertexId> = BTreeSet::new();
        for vid in &current {
            for eid in store.out_edge_ids(*vid, graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing path-index edge {eid}"))
                })?;
                if &edge.label == label {
                    next_set.insert(edge.target_id);
                }
            }
        }
        current = next_set;
        if current.is_empty() {
            break;
        }
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use uqa_core::{Value, Vertex};

    use super::VertexPropertyIndex;
    use crate::{GraphStore, MemoryGraphStore};

    #[test]
    fn vertex_property_index_tracks_selected_values_and_missing_fields() {
        let mut store = MemoryGraphStore::new();
        store.create_graph("g");
        let mut first = Vertex::new(1, "node");
        first.properties.insert("val".into(), Value::Int(7));
        first.properties.insert("nullable".into(), Value::Null);
        store.add_vertex(first, "g").unwrap();
        let mut second = Vertex::new(2, "node");
        second.properties.insert("val".into(), Value::Float(7.0));
        store.add_vertex(second, "g").unwrap();

        let index =
            VertexPropertyIndex::build(&store, "g", &["val", "nullable", "missing"]).unwrap();

        assert!(index.has_property("val"));
        assert!(index.has_property("missing"));
        assert_eq!(
            index.lookup_eq("val", &Value::Int(7)).unwrap(),
            &std::collections::BTreeSet::from([1, 2])
        );
        assert_eq!(
            index.lookup_eq("nullable", &Value::Null).unwrap(),
            &std::collections::BTreeSet::from([1])
        );
        assert!(index.lookup_eq("missing", &Value::Null).is_none());
        assert!(index.lookup_eq("not-indexed", &Value::Int(7)).is_none());
    }

    #[test]
    fn vertex_property_index_rejects_unknown_graph() {
        let store = MemoryGraphStore::new();
        let error = VertexPropertyIndex::build(&store, "missing", &["val"]).unwrap_err();
        assert!(error.to_string().contains("missing"));
    }
}
