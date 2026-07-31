//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit adapters between posting representations.
//!
//! These transformations map values only. They are intentionally not called
//! functors: no operator/morphism map is defined, so category identity and
//! composition laws are not part of their contract.

use std::collections::BTreeMap;

use uqa_core::{Payload, PostingEntry, PostingList};

use crate::posting_list::{GraphPayload, GraphPostingList, GraphPostingListResult};

/// Versioned codec between graph side-table storage and an ordinary posting
/// payload. Encoding preserves graph metadata rather than stripping it.
pub struct GraphPostingCodec;

impl GraphPostingCodec {
    #[allow(clippy::needless_pass_by_value)]
    pub fn encode(graph: GraphPostingList) -> PostingList {
        graph.to_posting_list()
    }

    pub fn decode(posting: &PostingList) -> GraphPostingList {
        GraphPostingList::from_posting_list(posting)
    }
}

/// Attach a shared vertex-context side table to every posting entry.
///
/// This adapter does not invent graph edges, so it has no edge-label option.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostingToGraphAdapter;

impl PostingToGraphAdapter {
    #[allow(clippy::needless_pass_by_value)]
    pub fn attach_shared_vertex_context(
        &self,
        posting: PostingList,
    ) -> GraphPostingListResult<GraphPostingList> {
        let all_vertices: Vec<u64> = posting.iter().map(|entry| entry.doc_id).collect();
        let graph_payloads: BTreeMap<u64, GraphPayload> = posting
            .iter()
            .map(|entry| {
                (
                    entry.doc_id,
                    GraphPayload {
                        subgraph_vertices: all_vertices.clone(),
                        subgraph_edges: Vec::new(),
                        graph_name: String::new(),
                        score_override: Some(entry.payload.score),
                    },
                )
            })
            .collect();
        GraphPostingList::try_from_parts(posting, graph_payloads)
    }
}

/// Normalize a TF-weighted text score into `[0, 1]` over one posting list.
///
/// This is a query-pool score transform; it does not construct vectors and
/// therefore has no vector-dimension setting.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextTfScoreNormalizer;

impl TextTfScoreNormalizer {
    #[allow(clippy::needless_pass_by_value)]
    pub fn normalize(&self, posting: PostingList) -> PostingList {
        if posting.is_empty() {
            return PostingList::new();
        }
        let mut raw_scores: Vec<(PostingEntry, f64)> = Vec::with_capacity(posting.len());
        let mut max_score = 0.0_f64;
        for entry in &posting {
            let term_frequency = if entry.payload.positions.is_empty() {
                1
            } else {
                entry.payload.positions.len()
            };
            let raw = term_frequency as f64 * entry.payload.score.max(0.01);
            max_score = max_score.max(raw);
            raw_scores.push((entry.clone(), raw));
        }
        let entries = raw_scores
            .into_iter()
            .map(|(entry, raw)| PostingEntry {
                doc_id: entry.doc_id,
                payload: Payload {
                    positions: entry.payload.positions,
                    score: if max_score > 0.0 {
                        raw / max_score
                    } else {
                        0.0
                    },
                    fields: entry.payload.fields,
                },
            })
            .collect();
        PostingList::from_sorted_unchecked(entries)
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
    fn posting_adapter_attaches_vertices_without_inventing_edges() {
        let posting = PostingList::from_sorted_unchecked(vec![
            entry(1, 0.5, Vec::new()),
            entry(2, 0.7, Vec::new()),
        ]);
        let graph = PostingToGraphAdapter
            .attach_shared_vertex_context(posting)
            .unwrap();
        let payload = graph.get_graph_payload(1).unwrap();
        assert_eq!(payload.subgraph_vertices, vec![1, 2]);
        assert!(payload.subgraph_edges.is_empty());
        assert_eq!(payload.score_override, Some(0.5));
    }

    #[test]
    fn text_score_normalizer_normalizes_scores_without_vector_metadata() {
        let posting = PostingList::from_sorted_unchecked(vec![
            entry(1, 0.5, vec![1, 2]),
            entry(2, 0.5, vec![1]),
        ]);
        let mapped = TextTfScoreNormalizer.normalize(posting);
        let scores: Vec<f64> = mapped.iter().map(|entry| entry.payload.score).collect();
        assert!(scores.iter().any(|score| (score - 1.0).abs() < 1e-9));
        assert!(scores.iter().all(|score| (0.0..=1.0).contains(score)));
    }

    #[test]
    fn graph_codec_round_trips_complete_graph_payloads() {
        let posting = PostingList::from_sorted_unchecked(vec![entry(1, 0.5, Vec::new())]);
        let graph = PostingToGraphAdapter
            .attach_shared_vertex_context(posting)
            .unwrap();
        let encoded = GraphPostingCodec::encode(graph.clone());
        assert_eq!(GraphPostingCodec::decode(&encoded), graph);
    }
}
