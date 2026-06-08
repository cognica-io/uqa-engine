//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vertex centrality measures: `PageRank`, `HITS`, betweenness. Each
//! operator runs against a [`GraphStore`] and returns a
//! [`GraphPostingList`] keyed on vertex id with a calibrated score.

use std::collections::{BTreeMap, VecDeque};

use uqa_core::{DocId, Payload, PostingEntry, PostingList, Value, VertexId};

use crate::posting_list::{GraphPayload, GraphPostingList};
use crate::store::GraphStore;

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

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let vertices: Vec<VertexId> = store.vertex_ids_in_graph(self.graph).into_iter().collect();
        let n = vertices.len();
        if n == 0 {
            return GraphPostingList::new();
        }
        if n == 1 {
            let vid = vertices[0];
            return single_vertex_result(vid, 1.0, &vertices, self.graph);
        }

        let mut rank: BTreeMap<VertexId, f64> =
            vertices.iter().map(|v| (*v, 1.0 / n as f64)).collect();
        let mut out_degree: BTreeMap<VertexId, usize> = BTreeMap::new();
        let mut in_neighbors: BTreeMap<VertexId, Vec<VertexId>> = BTreeMap::new();
        for v in &vertices {
            out_degree.insert(*v, store.out_edge_ids(*v, self.graph).len());
            let mut ins: Vec<VertexId> = Vec::new();
            for eid in store.in_edge_ids(*v, self.graph) {
                if let Some(edge) = store.get_edge(eid) {
                    ins.push(edge.source_id);
                }
            }
            in_neighbors.insert(*v, ins);
        }

        let d = self.damping;
        for _ in 0..self.max_iterations {
            let mut new_rank: BTreeMap<VertexId, f64> = BTreeMap::new();
            for v in &vertices {
                let mut incoming = 0.0;
                if let Some(ins) = in_neighbors.get(v) {
                    for u in ins {
                        let deg = *out_degree.get(u).unwrap_or(&0);
                        if deg > 0 {
                            incoming += rank[u] / deg as f64;
                        }
                    }
                }
                new_rank.insert(*v, (1.0 - d) / n as f64 + d * incoming);
            }
            let delta: f64 = vertices.iter().map(|v| (new_rank[v] - rank[v]).abs()).sum();
            rank = new_rank;
            if delta < self.tolerance {
                break;
            }
        }

        let normalized = min_max_normalize(&rank, &vertices);
        build_score_result(&vertices, &normalized, &[], self.graph, &BTreeMap::new())
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

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let vertices: Vec<VertexId> = store.vertex_ids_in_graph(self.graph).into_iter().collect();
        if vertices.is_empty() {
            return GraphPostingList::new();
        }

        let mut hub: BTreeMap<VertexId, f64> = vertices.iter().map(|v| (*v, 1.0)).collect();
        let mut auth: BTreeMap<VertexId, f64> = vertices.iter().map(|v| (*v, 1.0)).collect();
        let mut in_neighbors: BTreeMap<VertexId, Vec<VertexId>> = BTreeMap::new();
        let mut out_neighbors: BTreeMap<VertexId, Vec<VertexId>> = BTreeMap::new();
        for v in &vertices {
            let mut ins = Vec::new();
            for eid in store.in_edge_ids(*v, self.graph) {
                if let Some(edge) = store.get_edge(eid) {
                    ins.push(edge.source_id);
                }
            }
            in_neighbors.insert(*v, ins);
            let mut outs = Vec::new();
            for eid in store.out_edge_ids(*v, self.graph) {
                if let Some(edge) = store.get_edge(eid) {
                    outs.push(edge.target_id);
                }
            }
            out_neighbors.insert(*v, outs);
        }

        for _ in 0..self.max_iterations {
            let mut new_auth: BTreeMap<VertexId, f64> = BTreeMap::new();
            for v in &vertices {
                let s = in_neighbors[v].iter().map(|u| hub[u]).sum::<f64>();
                new_auth.insert(*v, s);
            }
            let mut new_hub: BTreeMap<VertexId, f64> = BTreeMap::new();
            for v in &vertices {
                let s = out_neighbors[v].iter().map(|w| new_auth[w]).sum::<f64>();
                new_hub.insert(*v, s);
            }
            let auth_norm = new_auth.values().map(|x| x * x).sum::<f64>().sqrt();
            let hub_norm = new_hub.values().map(|x| x * x).sum::<f64>().sqrt();
            if auth_norm > 0.0 {
                for v in &vertices {
                    *new_auth.get_mut(v).unwrap() /= auth_norm;
                }
            }
            if hub_norm > 0.0 {
                for v in &vertices {
                    *new_hub.get_mut(v).unwrap() /= hub_norm;
                }
            }
            let delta: f64 = vertices
                .iter()
                .map(|v| (new_auth[v] - auth[v]).abs() + (new_hub[v] - hub[v]).abs())
                .sum();
            auth = new_auth;
            hub = new_hub;
            if delta < self.tolerance {
                break;
            }
        }

        let auth_n = min_max_normalize(&auth, &vertices);
        let hub_n = min_max_normalize(&hub, &vertices);
        let mut extra_fields: BTreeMap<VertexId, BTreeMap<String, Value>> = BTreeMap::new();
        for v in &vertices {
            let mut m: BTreeMap<String, Value> = BTreeMap::new();
            m.insert("hub_score".into(), Value::Float(hub_n[v]));
            m.insert("authority_score".into(), Value::Float(auth_n[v]));
            extra_fields.insert(*v, m);
        }
        build_score_result(&vertices, &auth_n, &[], self.graph, &extra_fields)
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

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphPostingList {
        let vertices: Vec<VertexId> = store.vertex_ids_in_graph(self.graph).into_iter().collect();
        let n = vertices.len();
        if n == 0 {
            return GraphPostingList::new();
        }
        if n == 1 {
            return single_vertex_result(vertices[0], 0.0, &vertices, self.graph);
        }

        let vertex_index: BTreeMap<VertexId, usize> = vertices
            .iter()
            .enumerate()
            .map(|(idx, vertex_id)| (*vertex_id, idx))
            .collect();
        let mut out_neighbors: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (idx, vertex_id) in vertices.iter().enumerate() {
            for eid in store.out_edge_ids(*vertex_id, self.graph) {
                if let Some(edge) = store.get_edge(eid) {
                    if let Some(target_idx) = vertex_index.get(&edge.target_id) {
                        out_neighbors[idx].push(*target_idx);
                    }
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
                        dist[w] = dist[v] + 1;
                        queue.push_back(w);
                    }
                    if dist[w] == dist[v] + 1 {
                        sigma[w] += sigma[v];
                        predecessors[w].push(v);
                    }
                }
            }
            let mut delta = vec![0.0; n];
            while let Some(w) = stack.pop() {
                for &v in &predecessors[w] {
                    if sigma[w] > 0 {
                        let contrib = (sigma[v] as f64 / sigma[w] as f64) * (1.0 + delta[w]);
                        delta[v] += contrib;
                    }
                }
                if w != s {
                    cb[w] += delta[w];
                }
            }
        }

        let normalization = ((n - 1) * n.saturating_sub(2)) as f64;
        if normalization > 0.0 {
            for value in &mut cb {
                *value /= normalization;
            }
        }
        let cb: BTreeMap<VertexId, f64> = vertices
            .iter()
            .zip(cb)
            .map(|(vertex_id, score)| (*vertex_id, score.clamp(0.0, 1.0)))
            .collect();
        build_score_result(&vertices, &cb, &[], self.graph, &BTreeMap::new())
    }
}

fn min_max_normalize(
    scores: &BTreeMap<VertexId, f64>,
    vertices: &[VertexId],
) -> BTreeMap<VertexId, f64> {
    let min_s = scores.values().copied().fold(f64::INFINITY, f64::min);
    let max_s = scores.values().copied().fold(f64::NEG_INFINITY, f64::max);
    if max_s - min_s > 0.0 {
        vertices
            .iter()
            .map(|v| (*v, (scores[v] - min_s) / (max_s - min_s)))
            .collect()
    } else {
        vertices.iter().map(|v| (*v, 1.0)).collect()
    }
}

fn single_vertex_result(
    vid: VertexId,
    score: f64,
    vertices: &[VertexId],
    graph: &str,
) -> GraphPostingList {
    let entry = PostingEntry::new(vid, Payload::with_score(score));
    let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
    graph_payloads.insert(
        vid,
        GraphPayload {
            subgraph_vertices: vertices.to_vec(),
            subgraph_edges: Vec::new(),
            graph_name: graph.to_string(),
            score_override: Some(score),
        },
    );
    GraphPostingList::from_parts(
        PostingList::from_sorted_unchecked(vec![entry]),
        graph_payloads,
    )
}

fn build_score_result(
    vertices: &[VertexId],
    scores: &BTreeMap<VertexId, f64>,
    _edges: &[u64],
    graph: &str,
    extra_fields: &BTreeMap<VertexId, BTreeMap<String, Value>>,
) -> GraphPostingList {
    let mut entries: Vec<PostingEntry> = Vec::with_capacity(vertices.len());
    let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
    let mut sorted = vertices.to_vec();
    sorted.sort_unstable();
    for vid in &sorted {
        let score = *scores.get(vid).unwrap_or(&0.0);
        let mut payload = Payload::with_score(score);
        if let Some(fields) = extra_fields.get(vid) {
            payload.fields = fields.clone();
        }
        entries.push(PostingEntry::new(*vid, payload));
        graph_payloads.insert(
            *vid,
            GraphPayload {
                subgraph_vertices: sorted.clone(),
                subgraph_edges: Vec::new(),
                graph_name: graph.to_string(),
                score_override: Some(score),
            },
        );
    }
    GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
}
