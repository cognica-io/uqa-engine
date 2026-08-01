//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded weighted regular-path execution.

use super::{
    build_nfa, simplify, subset_construction, value_as_f64, BTreeMap, Dfa, DfaState, EdgeId,
    GraphPayload, GraphPostingList, GraphStore, GraphStoreError, GraphStoreResult,
    PathWeightPredicate, Payload, PostingEntry, PostingList, RegularPathExpr, Value, VecDeque,
    VertexId, DEFAULT_GRAPH_SCORE,
};

/// A bounded regular-path walk whose accumulated numeric edge weight must
/// satisfy a caller-provided predicate.
///
/// Unlike reachability-only RPQ evaluation, weighted execution cannot collapse
/// all visits to the same `(vertex, DFA state)`: two walks can reach that
/// configuration with different accumulated weights. `max_hops` therefore
/// makes the walk domain explicit and finite. When several accepted walks end
/// at the same vertex, the result retains the greatest accumulated weight and
/// its concrete vertex / edge path.
pub struct WeightedPathQuery<'a> {
    pub path: RegularPathExpr,
    pub graph: &'a str,
    pub start_vertex: Option<VertexId>,
    pub weight_property: &'a str,
    pub default_edge_weight: f64,
    pub max_hops: usize,
    pub predicate: PathWeightPredicate,
    pub score: f64,
}

impl<'a> WeightedPathQuery<'a> {
    pub fn new(
        path: RegularPathExpr,
        graph: &'a str,
        weight_property: &'a str,
        predicate: PathWeightPredicate,
    ) -> Self {
        Self {
            path,
            graph,
            start_vertex: None,
            weight_property,
            default_edge_weight: 1.0,
            max_hops: 16,
            predicate,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    pub fn from_vertex(mut self, start: VertexId) -> Self {
        self.start_vertex = Some(start);
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        if !self.default_edge_weight.is_finite() {
            return Err(GraphStoreError::InvalidMutation(format!(
                "default edge weight must be finite, got {}",
                self.default_edge_weight
            )));
        }
        if !self.score.is_finite() {
            return Err(GraphStoreError::InvalidMutation(format!(
                "weighted path score must be finite, got {}",
                self.score
            )));
        }
        let simplified = simplify(&self.path)
            .map_err(|error| GraphStoreError::InvalidQuery(error.to_string()))?;
        let nfa = build_nfa(&simplified)
            .map_err(|error| GraphStoreError::InvalidQuery(error.to_string()))?;
        let dfa = subset_construction(&nfa)
            .map_err(|error| GraphStoreError::InvalidQuery(error.to_string()))?;
        let starts: Vec<VertexId> = match self.start_vertex {
            Some(vertex) => {
                store.require_vertex_in_graph(vertex, self.graph)?;
                vec![vertex]
            }
            None => store.vertex_ids_in_graph(self.graph)?.into_iter().collect(),
        };
        let mut accepted = BTreeMap::<VertexId, WeightedPathMatch>::new();
        for start in starts {
            self.simulate_from(store, start, &dfa, &mut accepted)?;
        }

        let mut entries = Vec::with_capacity(accepted.len());
        let mut graph_payloads = BTreeMap::new();
        for (end, path_match) in accepted {
            let mut fields = BTreeMap::new();
            fields.insert("_path_weight".to_string(), Value::Float(path_match.weight));
            entries.push(PostingEntry::new(
                end,
                Payload {
                    score: self.score,
                    fields,
                    ..Default::default()
                },
            ));
            graph_payloads.insert(
                end,
                GraphPayload {
                    subgraph_vertices: path_match.vertices,
                    subgraph_edges: path_match.edges,
                    graph_name: self.graph.to_string(),
                    score_override: Some(self.score),
                },
            );
        }
        GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        )
        .map_err(Into::into)
    }

    fn simulate_from<G: GraphStore>(
        &self,
        store: &G,
        start: VertexId,
        dfa: &Dfa,
        accepted: &mut BTreeMap<VertexId, WeightedPathMatch>,
    ) -> GraphStoreResult<()> {
        let mut queue = VecDeque::from([WeightedWalk {
            vertex: start,
            state: dfa.start.clone(),
            hops: 0,
            weight: 0.0,
            vertices: vec![start],
            edges: Vec::new(),
        }]);
        if dfa.accepts.contains(&dfa.start) && (self.predicate)(0.0) {
            record_weighted_match(accepted, start, 0.0, vec![start], Vec::new());
        }

        while let Some(walk) = queue.pop_front() {
            if walk.hops >= self.max_hops {
                continue;
            }
            let Some(transitions) = dfa.transitions.get(&walk.state) else {
                continue;
            };
            for edge_id in store.out_edge_ids(walk.vertex, self.graph)? {
                let edge = store.get_edge(edge_id).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing weighted-path edge {edge_id}"))
                })?;
                let Some(next_state) = transitions.get(&edge.label) else {
                    continue;
                };
                let edge_weight = match edge.properties.get(self.weight_property) {
                    Some(value) => value_as_f64(value)?.ok_or_else(|| {
                        GraphStoreError::InvalidMutation(format!(
                            "edge {edge_id} weight property {:?} is not numeric",
                            self.weight_property
                        ))
                    })?,
                    None => self.default_edge_weight,
                };
                let weight = walk.weight + edge_weight;
                if !weight.is_finite() {
                    return Err(GraphStoreError::InvalidMutation(format!(
                        "weighted path accumulation is not finite at edge {edge_id}"
                    )));
                }
                let mut vertices = walk.vertices.clone();
                vertices.push(edge.target_id);
                let mut edges = walk.edges.clone();
                edges.push(edge_id);
                if dfa.accepts.contains(next_state) && (self.predicate)(weight) {
                    record_weighted_match(
                        accepted,
                        edge.target_id,
                        weight,
                        vertices.clone(),
                        edges.clone(),
                    );
                }
                queue.push_back(WeightedWalk {
                    vertex: edge.target_id,
                    state: next_state.clone(),
                    hops: walk.hops.checked_add(1).ok_or_else(|| {
                        GraphStoreError::CorruptGraph("weighted path hop count overflow".into())
                    })?,
                    weight,
                    vertices,
                    edges,
                });
            }
        }
        Ok(())
    }
}

struct WeightedWalk {
    vertex: VertexId,
    state: DfaState,
    hops: usize,
    weight: f64,
    vertices: Vec<VertexId>,
    edges: Vec<EdgeId>,
}

struct WeightedPathMatch {
    weight: f64,
    vertices: Vec<VertexId>,
    edges: Vec<EdgeId>,
}

fn record_weighted_match(
    accepted: &mut BTreeMap<VertexId, WeightedPathMatch>,
    endpoint: VertexId,
    weight: f64,
    vertices: Vec<VertexId>,
    edges: Vec<EdgeId>,
) {
    let replace = accepted
        .get(&endpoint)
        .is_none_or(|current| weight.total_cmp(&current.weight).is_gt());
    if replace {
        accepted.insert(
            endpoint,
            WeightedPathMatch {
                weight,
                vertices,
                edges,
            },
        );
    }
}
