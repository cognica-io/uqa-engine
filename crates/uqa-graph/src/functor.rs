//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Category-theoretic functors between UQA paradigms (Paper 1,
//! Section 7). 1:1 port of `uqa.core.functor`.
//!
//! Each paradigm (relational, text, vector, graph) is a category, and
//! a functor is a structure-preserving map between categories. The
//! functors here move objects (posting lists) and morphisms
//! (operators) across paradigm boundaries.
//!
//! Laws:
//! ```text
//! F(id_A) = id_{F(A)}      (identity preservation)
//! F(g . f) = F(g) . F(f)   (composition preservation)
//! ```

use std::collections::BTreeMap;

use uqa_core::{Payload, PostingEntry, PostingList};

use crate::posting_list::{GraphPayload, GraphPostingList};

/// `Graph -> Relational` functor.
///
/// Maps a [`GraphPostingList`] to a plain [`PostingList`] by stripping
/// the per-doc graph payload metadata. A plain `PostingList` is
/// returned unchanged.
pub struct GraphToRelationalFunctor;

impl GraphToRelationalFunctor {
    #[allow(clippy::needless_pass_by_value)]
    pub fn map_object(obj: GraphPostingList) -> PostingList {
        obj.to_posting_list()
    }
}

/// `Relational -> Graph` functor.
///
/// Promotes every posting list entry to a graph vertex sharing a
/// common subgraph. Edge label is `edge_label` (default `adjacent`).
pub struct RelationalToGraphFunctor {
    pub edge_label: String,
}

impl RelationalToGraphFunctor {
    pub fn new(edge_label: impl Into<String>) -> Self {
        Self {
            edge_label: edge_label.into(),
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn map_object(&self, obj: PostingList) -> GraphPostingList {
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(obj.len());
        let mut graph_payloads: BTreeMap<u64, GraphPayload> = BTreeMap::new();
        let mut all_vids: Vec<u64> = Vec::with_capacity(obj.len());
        for entry in &obj {
            all_vids.push(entry.doc_id);
            entries.push(entry.clone());
        }
        for entry in &entries {
            graph_payloads.insert(
                entry.doc_id,
                GraphPayload {
                    subgraph_vertices: all_vids.clone(),
                    subgraph_edges: Vec::new(),
                    graph_name: String::new(),
                    score_override: Some(entry.payload.score),
                },
            );
        }
        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), graph_payloads)
    }
}

impl Default for RelationalToGraphFunctor {
    fn default() -> Self {
        Self::new("adjacent")
    }
}

/// `Text -> Vector` functor.
///
/// Promotes every text posting list entry to a vector-style score by
/// using the position count as a TF proxy and normalising the result
/// into `[0, 1]`. Mirrors the Python reference's two-pass algorithm
/// (compute raw scores, divide each by the max).
pub struct TextToVectorFunctor {
    pub dimensions: usize,
}

impl TextToVectorFunctor {
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn map_object(&self, obj: PostingList) -> PostingList {
        if obj.is_empty() {
            return PostingList::new();
        }
        let mut raw_scores: Vec<(PostingEntry, f64)> = Vec::with_capacity(obj.len());
        let mut max_score = 0.0_f64;
        for entry in &obj {
            let tf = if entry.payload.positions.is_empty() {
                1
            } else {
                entry.payload.positions.len()
            };
            let raw = tf as f64 * entry.payload.score.max(0.01);
            if raw > max_score {
                max_score = raw;
            }
            raw_scores.push((entry.clone(), raw));
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(raw_scores.len());
        for (entry, raw) in raw_scores {
            let normalized = if max_score > 0.0 {
                raw / max_score
            } else {
                0.0
            };
            entries.push(PostingEntry {
                doc_id: entry.doc_id,
                payload: Payload {
                    positions: entry.payload.positions.clone(),
                    score: normalized,
                    fields: entry.payload.fields.clone(),
                },
            });
        }
        PostingList::from_sorted_unchecked(entries)
    }
}

impl Default for TextToVectorFunctor {
    fn default() -> Self {
        Self::new(128)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(doc_id: u64, score: f64, positions: Vec<u32>) -> PostingEntry {
        PostingEntry {
            doc_id,
            payload: Payload {
                positions,
                score,
                fields: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn relational_to_graph_attaches_subgraph_payload() {
        let pl = PostingList::from_sorted_unchecked(vec![
            entry(1, 0.5, Vec::new()),
            entry(2, 0.7, Vec::new()),
        ]);
        let f = RelationalToGraphFunctor::new("knows");
        let gpl = f.map_object(pl);
        let payload = gpl.get_graph_payload(1).unwrap();
        assert!(payload.subgraph_vertices.contains(&1));
        assert!(payload.subgraph_vertices.contains(&2));
        assert_eq!(payload.score_override, Some(0.5));
    }

    #[test]
    fn text_to_vector_normalizes_scores() {
        let pl = PostingList::from_sorted_unchecked(vec![
            entry(1, 0.5, vec![1, 2]),
            entry(2, 0.5, vec![1]),
        ]);
        let f = TextToVectorFunctor::default();
        let mapped = f.map_object(pl);
        let scores: Vec<f64> = mapped.iter().map(|e| e.payload.score).collect();
        assert!(scores.iter().any(|s| (s - 1.0).abs() < 1e-9));
        assert!(scores.iter().all(|s| (0.0..=1.0).contains(s)));
    }

    #[test]
    fn graph_to_relational_strips_payloads() {
        let pl = PostingList::from_sorted_unchecked(vec![
            entry(1, 0.5, Vec::new()),
            entry(2, 0.7, Vec::new()),
        ]);
        let f = RelationalToGraphFunctor::default();
        let gpl = f.map_object(pl);
        let plain = GraphToRelationalFunctor::map_object(gpl);
        assert_eq!(plain.len(), 2);
    }
}
