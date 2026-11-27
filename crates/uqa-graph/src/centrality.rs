//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vertex centrality measures: `PageRank`, `HITS`, betweenness. Each
//! operator runs against a [`GraphStore`] and returns a
//! [`GraphPostingList`] keyed on vertex id with a calibrated score.

use std::sync::Arc;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use uqa_core::{Edge, EdgeId, Payload, PostingEntry, PostingList, Value, VertexId};

use crate::adjacency::{edge_endpoint, vertex_index};
use crate::posting_list::GraphPostingList;
use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};

const MAX_EXACT_F64_INTEGER: u64 = 9_007_199_254_740_992;

fn usize_as_f64(value: usize, context: &str) -> GraphStoreResult<f64> {
    let value = u64::try_from(value)
        .map_err(|_| GraphStoreError::CorruptGraph(format!("{context} exceeds the u64 range")))?;
    u64_as_f64(value, context)
}

fn u64_as_f64(value: u64, context: &str) -> GraphStoreResult<f64> {
    if value <= MAX_EXACT_F64_INTEGER {
        Ok(value as f64)
    } else {
        Err(GraphStoreError::CorruptGraph(format!(
            "{context} {value} exceeds the exact f64 integer range"
        )))
    }
}

/// `PageRank` centrality (power iteration with damping).
///
/// Iterates `new_rank[v] = (1 - d)/N + d * sum(rank[u] / out_deg(u))`
/// over in-neighbors `u`, until the L1 delta drops below
/// `tolerance` or `max_iterations` is reached. Final scores are
/// min-max normalized to `[0, 1]`.
pub struct PageRank<'a> {
    pub graph: &'a str,
    pub damping: f64,
    pub max_iterations: u32,
    pub tolerance: f64,
}

impl<'a> PageRank<'a> {
    pub fn new(graph: &'a str) -> Self {
        Self {
            graph,
            damping: 0.85,
            max_iterations: 100,
            tolerance: 1e-6,
        }
    }

    pub fn damping(mut self, d: f64) -> Self {
        self.damping = d;
        self
    }

    pub fn max_iterations(mut self, k: u32) -> Self {
        self.max_iterations = k;
        self
    }

    pub fn tolerance(mut self, t: f64) -> Self {
        self.tolerance = t;
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        if !self.damping.is_finite() || !(0.0..=1.0).contains(&self.damping) {
            return Err(GraphStoreError::InvalidMutation(format!(
                "PageRank damping must be finite and in [0, 1], got {}",
                self.damping
            )));
        }
        if !self.tolerance.is_finite() || self.tolerance < 0.0 {
            return Err(GraphStoreError::InvalidMutation(format!(
                "PageRank tolerance must be finite and non-negative, got {}",
                self.tolerance
            )));
        }
        let vertices: Vec<VertexId> = store.vertex_ids_in_graph(self.graph)?.into_iter().collect();
        let n = vertices.len();
        if n == 0 {
            return Ok(GraphPostingList::new());
        }
        if n == 1 {
            return Ok(build_score_result(vertices, self.graph, |_| {
                Payload::with_score(1.0)
            }));
        }

        let n_f64 = usize_as_f64(n, "PageRank vertex count")?;
        let mut out_degree = Vec::with_capacity(n);
        let mut incoming = Vec::with_capacity(n);
        for &id in &vertices {
            let degree = store.out_edge_ids(id, self.graph)?.len();
            out_degree.push(usize_as_f64(degree, "PageRank out-degree")?);
            let mut sources = Vec::new();
            for eid in store.in_edge_ids(id, self.graph)? {
                let edge = store.get_edge(eid)?.ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing PageRank edge {eid}"))
                })?;
                if let Ok(index) = vertices.binary_search(&edge.source_id) {
                    sources.push(index);
                }
            }
            incoming.push(sources);
        }

        let d = self.damping;
        let mut rank = vec![1.0 / n_f64; n];
        let mut next = vec![0.0; n];
        for _ in 0..self.max_iterations {
            for (result, sources) in next.iter_mut().zip(&incoming) {
                let mut total = 0.0;
                for &source in sources {
                    if out_degree[source] > 0.0 {
                        total += rank[source] / out_degree[source];
                    }
                }
                *result = (1.0 - d) / n_f64 + d * total;
            }
            let delta: f64 = next.iter().zip(&rank).map(|(a, b)| (a - b).abs()).sum();
            std::mem::swap(&mut rank, &mut next);
            if delta < self.tolerance {
                break;
            }
        }
        min_max_normalize(&mut rank);
        Ok(build_score_result(vertices, self.graph, |index| {
            Payload::with_score(rank[index])
        }))
    }
}

/// `HITS` centrality (hub / authority mutual reinforcement).
///
/// Authority of `v` is the sum of hub scores of in-neighbors; hub is
/// the sum of authority scores of out-neighbors. Each round normalizes
/// by L2 norm. Final hub and authority scores are min-max normalized
/// to `[0, 1]`. The payload's `score` is the authority; the per-entry
/// fields carry both `hub_score` and `authority_score`.
pub struct HITS<'a> {
    pub graph: &'a str,
    pub max_iterations: u32,
    pub tolerance: f64,
}

impl<'a> HITS<'a> {
    pub fn new(graph: &'a str) -> Self {
        Self {
            graph,
            max_iterations: 100,
            tolerance: 1e-6,
        }
    }

    pub fn max_iterations(mut self, k: u32) -> Self {
        self.max_iterations = k;
        self
    }

    pub fn tolerance(mut self, t: f64) -> Self {
        self.tolerance = t;
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        if !self.tolerance.is_finite() || self.tolerance < 0.0 {
            return Err(GraphStoreError::InvalidMutation(format!(
                "HITS tolerance must be finite and non-negative, got {}",
                self.tolerance
            )));
        }
        let vertices: Vec<VertexId> = store.vertex_ids_in_graph(self.graph)?.into_iter().collect();
        if vertices.is_empty() {
            return Ok(GraphPostingList::new());
        }

        let n = vertices.len();
        let mut incoming = Vec::with_capacity(n);
        let mut outgoing = Vec::with_capacity(n);
        let neighbors = |edges: BTreeSet<EdgeId>,
                         endpoint: fn(&Edge) -> VertexId|
         -> GraphStoreResult<Vec<usize>> {
            let mut indices = Vec::with_capacity(if self.max_iterations == 0 {
                0
            } else {
                edges.len()
            });
            for eid in edges {
                let id = edge_endpoint(store, "HITS", eid, endpoint)?;
                if self.max_iterations > 0 {
                    indices.push(vertex_index(&vertices, id, self.graph, eid)?);
                }
            }
            Ok(indices)
        };
        for &id in &vertices {
            incoming.push(neighbors(store.in_edge_ids(id, self.graph)?, |edge| {
                edge.source_id
            })?);
            outgoing.push(neighbors(store.out_edge_ids(id, self.graph)?, |edge| {
                edge.target_id
            })?);
        }

        let mut hub = vec![1.0; n];
        let mut auth = vec![1.0; n];
        let mut next_hub = vec![0.0; n];
        let mut next_auth = vec![0.0; n];
        for _ in 0..self.max_iterations {
            for (value, sources) in next_auth.iter_mut().zip(&incoming) {
                *value = sources.iter().map(|&index| hub[index]).sum();
            }
            for (value, targets) in next_hub.iter_mut().zip(&outgoing) {
                *value = targets.iter().map(|&index| next_auth[index]).sum();
            }
            for scores in [&mut next_auth, &mut next_hub] {
                let norm = scores.iter().map(|x| x * x).sum::<f64>().sqrt();
                if norm > 0.0 {
                    for score in scores {
                        *score /= norm;
                    }
                }
            }
            let delta: f64 = (0..n)
                .map(|i| (next_auth[i] - auth[i]).abs() + (next_hub[i] - hub[i]).abs())
                .sum();
            std::mem::swap(&mut auth, &mut next_auth);
            std::mem::swap(&mut hub, &mut next_hub);
            if delta < self.tolerance {
                break;
            }
        }
        min_max_normalize(&mut auth);
        min_max_normalize(&mut hub);
        Ok(build_score_result(vertices, self.graph, |index| {
            let mut payload = Payload::with_score(auth[index]);
            payload
                .fields
                .insert("hub_score".into(), Value::Float(hub[index]));
            payload
                .fields
                .insert("authority_score".into(), Value::Float(auth[index]));
            payload
        }))
    }
}

/// Betweenness centrality via Brandes algorithm.
///
/// For unweighted directed graphs, the per-vertex betweenness is
/// `sum over s != v != t of (sigma_st(v) / sigma_st)`. Scores are
/// normalized by `(N-1)*(N-2)` and clamped into `[0, 1]`.
pub struct BetweennessCentrality<'a> {
    pub graph: &'a str,
}

impl<'a> BetweennessCentrality<'a> {
    pub fn new(graph: &'a str) -> Self {
        Self { graph }
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        let vertices: Vec<VertexId> = store.vertex_ids_in_graph(self.graph)?.into_iter().collect();
        let n = vertices.len();
        if n == 0 {
            return Ok(GraphPostingList::new());
        }
        if n == 1 {
            return Ok(build_score_result(vertices, self.graph, |_| {
                Payload::with_score(0.0)
            }));
        }

        let vertex_index: BTreeMap<VertexId, usize> = vertices
            .iter()
            .enumerate()
            .map(|(idx, vertex_id)| (*vertex_id, idx))
            .collect();
        let mut out_neighbors: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (idx, vertex_id) in vertices.iter().enumerate() {
            for eid in store.out_edge_ids(*vertex_id, self.graph)? {
                let edge = store.get_edge(eid)?.ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing betweenness edge {eid}"))
                })?;
                if let Some(target_idx) = vertex_index.get(&edge.target_id) {
                    out_neighbors[idx].push(*target_idx);
                }
            }
        }

        let mut cb = vec![0.0; n];
        for s in 0..n {
            let mut stack: Vec<usize> = Vec::with_capacity(n);
            let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); n];
            let mut sigma = vec![0u64; n];
            sigma[s] = 1;
            let mut dist = vec![-1i64; n];
            dist[s] = 0;
            let mut queue: VecDeque<usize> = VecDeque::new();
            queue.push_back(s);
            while let Some(v) = queue.pop_front() {
                stack.push(v);
                for &w in &out_neighbors[v] {
                    if dist[w] < 0 {
                        dist[w] = dist[v].checked_add(1).ok_or_else(|| {
                            GraphStoreError::CorruptGraph(
                                "betweenness path distance exceeds bigint range".into(),
                            )
                        })?;
                        queue.push_back(w);
                    }
                    if dist[w] == dist[v] + 1 {
                        sigma[w] = sigma[w].checked_add(sigma[v]).ok_or_else(|| {
                            GraphStoreError::CorruptGraph(
                                "betweenness shortest-path count exceeds u64".into(),
                            )
                        })?;
                        predecessors[w].push(v);
                    }
                }
            }
            let mut delta = vec![0.0; n];
            while let Some(w) = stack.pop() {
                for &v in &predecessors[w] {
                    if sigma[w] > 0 {
                        let contrib = (u64_as_f64(sigma[v], "betweenness path count")?
                            / u64_as_f64(sigma[w], "betweenness path count")?)
                            * (1.0 + delta[w]);
                        delta[v] += contrib;
                    }
                }
                if w != s {
                    cb[w] += delta[w];
                }
            }
        }

        let normalization_count = (n - 1).checked_mul(n - 2).ok_or_else(|| {
            GraphStoreError::CorruptGraph("betweenness normalization count overflow".into())
        })?;
        let normalization = usize_as_f64(normalization_count, "betweenness normalization")?;
        if normalization > 0.0 {
            for value in &mut cb {
                *value /= normalization;
            }
        }
        Ok(build_score_result(vertices, self.graph, |index| {
            Payload::with_score(cb[index].clamp(0.0, 1.0))
        }))
    }
}

fn min_max_normalize(scores: &mut [f64]) {
    let min = scores.iter().copied().fold(f64::INFINITY, f64::min);
    let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if max - min > 0.0 {
        for score in scores {
            *score = (*score - min) / (max - min);
        }
    } else {
        scores.fill(1.0);
    }
}

fn build_score_result(
    vertices: Vec<VertexId>,
    graph: &str,
    payload: impl Fn(usize) -> Payload,
) -> GraphPostingList {
    // GraphStore returns vertex IDs in sorted order.
    let entries = vertices
        .iter()
        .enumerate()
        .map(|(index, &id)| PostingEntry::new(id, payload(index)))
        .collect();
    GraphPostingList::with_shared_subgraph(
        PostingList::from_sorted_unchecked(entries),
        vertices.into(),
        Arc::default(),
        graph.into(),
    )
}
