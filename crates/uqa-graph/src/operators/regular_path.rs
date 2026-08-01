//! DFA-backed regular path reachability.

use super::{
    build_nfa, simplify, subset_construction, BTreeMap, BTreeSet, Dfa, DfaState, DocId,
    GraphPayload, GraphPostingList, GraphStore, GraphStoreError, GraphStoreResult, Payload,
    PostingEntry, PostingList, RegularPathExpr, VecDeque, VertexId, DEFAULT_GRAPH_SCORE,
};

/// `RPQ_R` (Definition 5.1.2): evaluate a regular path expression over
/// a graph. The expression is simplified, compiled to an NFA via
/// Thompson's construction, then converted to a DFA and simulated by a
/// BFS over `(vertex, dfa-state)` configurations.
///
/// The result lists every endpoint vertex reachable from a start
/// vertex along a path matching the expression. Each endpoint becomes
/// one entry in the returned `GraphPostingList`.
pub struct RegularPathQuery<'a> {
    pub path: RegularPathExpr,
    pub graph: &'a str,
    /// `Some(start)` restricts evaluation to a single source. `None`
    /// runs the query from every vertex in the graph.
    pub start_vertex: Option<VertexId>,
    pub score: f64,
}

impl<'a> RegularPathQuery<'a> {
    pub fn new(path: RegularPathExpr, graph: &'a str) -> Self {
        Self {
            path,
            graph,
            start_vertex: None,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    pub fn from_vertex(mut self, start: VertexId) -> Self {
        self.start_vertex = Some(start);
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        let simplified = simplify(&self.path)
            .map_err(|error| GraphStoreError::InvalidQuery(error.to_string()))?;
        let nfa = build_nfa(&simplified)
            .map_err(|error| GraphStoreError::InvalidQuery(error.to_string()))?;
        let dfa = subset_construction(&nfa)
            .map_err(|error| GraphStoreError::InvalidQuery(error.to_string()))?;

        let starts: Vec<VertexId> = match self.start_vertex {
            Some(v) => {
                store.require_vertex_in_graph(v, self.graph)?;
                vec![v]
            }
            None => store.vertex_ids_in_graph(self.graph)?.into_iter().collect(),
        };

        let mut pairs: BTreeSet<(VertexId, VertexId)> = BTreeSet::new();
        for sv in &starts {
            self.simulate_from(store, *sv, &dfa, &mut pairs)?;
        }

        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        let mut seen: BTreeSet<DocId> = BTreeSet::new();
        for (start_v, end_v) in &pairs {
            let doc_id = *end_v;
            if seen.insert(doc_id) {
                entries.push(PostingEntry::new(doc_id, Payload::with_score(self.score)));
                let mut subgraph_vertices = vec![*start_v, *end_v];
                subgraph_vertices.sort_unstable();
                subgraph_vertices.dedup();
                graph_payloads.insert(
                    doc_id,
                    GraphPayload {
                        subgraph_vertices,
                        subgraph_edges: Vec::new(),
                        graph_name: self.graph.to_string(),
                        score_override: Some(self.score),
                    },
                );
            }
        }
        entries.sort_by_key(|e| e.doc_id);
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
        pairs: &mut BTreeSet<(VertexId, VertexId)>,
    ) -> GraphStoreResult<()> {
        let mut visited: BTreeSet<(VertexId, DfaState)> = BTreeSet::new();
        let mut queue: VecDeque<(VertexId, DfaState)> = VecDeque::new();
        queue.push_back((start, dfa.start.clone()));
        visited.insert((start, dfa.start.clone()));

        if dfa.accepts.contains(&dfa.start) {
            pairs.insert((start, start));
        }

        while let Some((vertex, state)) = queue.pop_front() {
            let Some(transitions) = dfa.transitions.get(&state) else {
                continue;
            };
            for eid in store.out_edge_ids(vertex, self.graph)? {
                let edge = store.get_edge(eid).ok_or_else(|| {
                    GraphStoreError::CorruptGraph(format!("missing RPQ edge {eid}"))
                })?;
                let Some(next_state) = transitions.get(&edge.label) else {
                    continue;
                };
                let neighbor = edge.target_id;
                if dfa.accepts.contains(next_state) {
                    pairs.insert((start, neighbor));
                }
                let key = (neighbor, next_state.clone());
                if visited.insert(key.clone()) {
                    queue.push_back(key);
                }
            }
        }
        Ok(())
    }
}
