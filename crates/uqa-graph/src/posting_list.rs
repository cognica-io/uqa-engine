//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph extension of [`uqa_core::PostingList`].
//!
//! `GraphPostingList` carries a `(doc_id -> GraphPayload)` side-table on
//! top of a standard posting list. The `Phi` homomorphism (Theorem
//! 1.1.6, Paper 2) shuttles that side-table through encoded field
//! entries on the underlying [`uqa_core::Payload`], so all of the
//! Boolean algebra from `PostingList` composes onto the graph algebra
//! without loss.

use std::collections::BTreeMap;

use uqa_core::{DocId, EdgeId, PostingEntry, PostingList, Value, VertexId};

const VERTICES_KEY: &str = "_graph_vertices";
const EDGES_KEY: &str = "_graph_edges";

/// Auxiliary payload for graph posting list entries: the subgraph each
/// matched entry refers to, plus a graph-name tag and a score override.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphPayload {
    pub subgraph_vertices: Vec<VertexId>,
    pub subgraph_edges: Vec<EdgeId>,
    pub graph_name: String,
    /// `Some(score)` overrides the underlying posting list's payload
    /// score during `Phi`. `None` keeps the standard payload score.
    pub score_override: Option<f64>,
}

impl GraphPayload {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Posting list paired with a `(doc_id -> GraphPayload)` map. The
/// invariants of the underlying `PostingList` (sorted, unique doc ids)
/// hold here as well.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphPostingList {
    inner: PostingList,
    graph_payloads: BTreeMap<DocId, GraphPayload>,
}

impl GraphPostingList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_parts(inner: PostingList, graph_payloads: BTreeMap<DocId, GraphPayload>) -> Self {
        Self {
            inner,
            graph_payloads,
        }
    }

    /// `Phi`: encode the graph payload map onto the underlying posting
    /// list's payload fields. The encoding writes sorted vertex / edge
    /// id lists into reserved field keys (`_graph_vertices`,
    /// `_graph_edges`) and lifts the override score onto the payload.
    /// The result composes with the standard posting-list algebra; to
    /// recover graph semantics, invert with `from_posting_list`.
    pub fn to_posting_list(&self) -> PostingList {
        let mut converted = Vec::with_capacity(self.inner.len());
        for entry in self.inner.entries() {
            let mut payload = entry.payload.clone();
            if let Some(gp) = self.graph_payloads.get(&entry.doc_id) {
                payload.fields.insert(
                    VERTICES_KEY.to_string(),
                    Value::List(
                        gp.subgraph_vertices
                            .iter()
                            .map(|v| Value::Int(*v as i64))
                            .collect(),
                    ),
                );
                payload.fields.insert(
                    EDGES_KEY.to_string(),
                    Value::List(
                        gp.subgraph_edges
                            .iter()
                            .map(|e| Value::Int(*e as i64))
                            .collect(),
                    ),
                );
                if let Some(score) = gp.score_override {
                    payload.score = score;
                }
            }
            converted.push(PostingEntry::new(entry.doc_id, payload));
        }
        PostingList::from_sorted_unchecked(converted)
    }

    /// `Phi^{-1}`: read encoded vertex/edge id lists out of payload
    /// fields and rebuild the `(doc_id -> GraphPayload)` side-table.
    /// Reserved keys are stripped from the resulting payloads.
    pub fn from_posting_list(pl: &PostingList) -> Self {
        let mut graph_payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        let mut entries = Vec::with_capacity(pl.len());
        for entry in pl.entries() {
            let vertices = decode_id_list(entry.payload.fields.get(VERTICES_KEY));
            let edges = decode_id_list(entry.payload.fields.get(EDGES_KEY));
            let has_graph_fields = entry.payload.fields.contains_key(VERTICES_KEY)
                || entry.payload.fields.contains_key(EDGES_KEY);
            let payload = if has_graph_fields {
                let mut p = entry.payload.clone();
                p.fields.remove(VERTICES_KEY);
                p.fields.remove(EDGES_KEY);
                p
            } else {
                entry.payload.clone()
            };
            if has_graph_fields {
                graph_payloads.insert(
                    entry.doc_id,
                    GraphPayload {
                        subgraph_vertices: vertices,
                        subgraph_edges: edges,
                        graph_name: String::new(),
                        score_override: Some(entry.payload.score),
                    },
                );
            }
            entries.push(PostingEntry::new(entry.doc_id, payload));
        }
        Self {
            inner: PostingList::from_sorted_unchecked(entries),
            graph_payloads,
        }
    }

    pub fn set_graph_payload(&mut self, doc_id: DocId, payload: GraphPayload) {
        self.graph_payloads.insert(doc_id, payload);
    }

    pub fn get_graph_payload(&self, doc_id: DocId) -> Option<&GraphPayload> {
        self.graph_payloads.get(&doc_id)
    }

    pub fn inner(&self) -> &PostingList {
        &self.inner
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Boolean union via `Phi`: round-trip both operands through the
    /// standard posting-list algebra and rebuild the graph view.
    pub fn union(&self, other: &Self) -> Self {
        let merged = self.to_posting_list().union(&other.to_posting_list());
        Self::from_posting_list(&merged)
    }

    pub fn intersect(&self, other: &Self) -> Self {
        let merged = self.to_posting_list().intersect(&other.to_posting_list());
        Self::from_posting_list(&merged)
    }

    pub fn difference(&self, other: &Self) -> Self {
        let merged = self.to_posting_list().difference(&other.to_posting_list());
        Self::from_posting_list(&merged)
    }
}

fn decode_id_list(value: Option<&Value>) -> Vec<u64> {
    let Some(Value::List(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| match v {
            Value::Int(n) if *n >= 0 => Some(*n as u64),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use uqa_core::Payload;

    use super::*;

    fn entry(doc_id: DocId, score: f64) -> PostingEntry {
        PostingEntry::new(doc_id, Payload::with_score(score))
    }

    fn fixture(doc_id: DocId, vertices: Vec<VertexId>, edges: Vec<EdgeId>) -> GraphPostingList {
        let mut gpl = GraphPostingList::from_parts(
            PostingList::from_sorted_unchecked(vec![entry(doc_id, 1.0)]),
            BTreeMap::new(),
        );
        gpl.set_graph_payload(
            doc_id,
            GraphPayload {
                subgraph_vertices: vertices,
                subgraph_edges: edges,
                ..GraphPayload::default()
            },
        );
        gpl
    }

    #[test]
    fn round_trip_preserves_graph_payload() {
        let gpl = fixture(7, vec![1, 2, 3], vec![10, 11]);
        let pl = gpl.to_posting_list();
        let restored = GraphPostingList::from_posting_list(&pl);
        let gp = restored.get_graph_payload(7).unwrap();
        assert_eq!(gp.subgraph_vertices, vec![1, 2, 3]);
        assert_eq!(gp.subgraph_edges, vec![10, 11]);
    }

    #[test]
    fn union_via_phi_merges_doc_ids() {
        let a = fixture(1, vec![1], vec![]);
        let b = fixture(2, vec![2], vec![]);
        let merged = a.union(&b);
        let ids: Vec<DocId> = merged.inner().doc_ids().collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(
            merged.get_graph_payload(1).unwrap().subgraph_vertices,
            vec![1]
        );
        assert_eq!(
            merged.get_graph_payload(2).unwrap().subgraph_vertices,
            vec![2]
        );
    }

    #[test]
    fn intersect_via_phi_keeps_only_shared() {
        let a = fixture(1, vec![1], vec![]);
        let b = fixture(1, vec![2], vec![]);
        let merged = a.intersect(&b);
        let ids: Vec<DocId> = merged.inner().doc_ids().collect();
        assert_eq!(ids, vec![1]);
    }

    #[test]
    fn difference_via_phi_removes_other() {
        let a = GraphPostingList::from_parts(
            PostingList::from_sorted_unchecked(vec![entry(1, 1.0), entry(2, 1.0)]),
            BTreeMap::new(),
        );
        let b = fixture(2, vec![], vec![]);
        let diff = a.difference(&b);
        let ids: Vec<DocId> = diff.inner().doc_ids().collect();
        assert_eq!(ids, vec![1]);
    }
}
