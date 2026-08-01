//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Label and predicate based single-vertex matching.

use super::{
    BTreeMap, DocId, GraphPayload, GraphPostingList, GraphStore, GraphStoreError, GraphStoreResult,
    Payload, PostingEntry, PostingList, VertexId, VertexPredicate, DEFAULT_GRAPH_SCORE,
};

/// Single-vertex Match: every vertex in `graph` whose label matches and
/// whose predicates all hold. Useful as a Cypher-style anchor.
pub struct VertexMatch<'a> {
    pub graph: &'a str,
    pub label: Option<&'a str>,
    pub predicate: Option<VertexPredicate>,
    pub score: f64,
}

impl<'a> VertexMatch<'a> {
    pub fn new(graph: &'a str) -> Self {
        Self {
            graph,
            label: None,
            predicate: None,
            score: DEFAULT_GRAPH_SCORE,
        }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn predicate(mut self, p: VertexPredicate) -> Self {
        self.predicate = Some(p);
        self
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        let candidates: Vec<VertexId> = match self.label {
            Some(l) => store.vertex_ids_by_label(l, self.graph)?,
            None => store.vertex_ids_in_graph(self.graph)?.into_iter().collect(),
        };
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for vid in candidates {
            let vtx = store.get_vertex(vid).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!("missing matched vertex {vid}"))
            })?;
            if let Some(pred) = &self.predicate {
                if !pred.matches(vtx) {
                    continue;
                }
            }
            entries.push(PostingEntry::new(vid, Payload::with_score(self.score)));
            graph_payloads.insert(
                vid,
                GraphPayload {
                    subgraph_vertices: vec![vid],
                    subgraph_edges: Vec::new(),
                    graph_name: self.graph.to_string(),
                    score_override: Some(self.score),
                },
            );
        }
        entries.sort_by_key(|e| e.doc_id);
        GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        )
        .map_err(Into::into)
    }
}

// -------------------------------------------------------------------------
// GMatch (subgraph isomorphism)
// -------------------------------------------------------------------------
