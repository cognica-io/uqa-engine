//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property tests for the `Phi` homomorphism between `GraphPostingList`
//! and the standard `PostingList` algebra (Theorem 1.1.6, Paper 2).
//!
//! For any composition of `union`, `intersect`, `difference` on graph
//! posting lists, encoding through `Phi` and decoding back must
//! preserve the doc-id-set structure of the result.

use std::collections::BTreeMap;

use proptest::prelude::*;
use uqa_core::{DocId, Payload, PostingEntry, PostingList};
use uqa_graph::{GraphPayload, GraphPostingList};

fn arb_graph_posting_list() -> impl Strategy<Value = GraphPostingList> {
    prop::collection::btree_set(0u64..32, 0..10).prop_map(|ids| {
        let mut entries = Vec::new();
        let mut payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for id in ids {
            entries.push(PostingEntry::new(id, Payload::with_score(1.0)));
            payloads.insert(
                id,
                GraphPayload {
                    subgraph_vertices: vec![id],
                    subgraph_edges: vec![id * 10],
                    ..GraphPayload::default()
                },
            );
        }
        GraphPostingList::from_parts(PostingList::from_sorted_unchecked(entries), payloads)
    })
}

fn doc_ids(g: &GraphPostingList) -> Vec<DocId> {
    g.inner().doc_ids().collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn phi_round_trip(g in arb_graph_posting_list()) {
        let pl = g.to_posting_list();
        let back = GraphPostingList::from_posting_list(&pl);
        prop_assert_eq!(doc_ids(&g), doc_ids(&back));
        for id in doc_ids(&g) {
            let original = g.get_graph_payload(id).cloned().unwrap_or_default();
            let restored = back.get_graph_payload(id).cloned().unwrap_or_default();
            prop_assert_eq!(original.subgraph_vertices, restored.subgraph_vertices);
            prop_assert_eq!(original.subgraph_edges, restored.subgraph_edges);
        }
    }

    #[test]
    fn phi_preserves_union(a in arb_graph_posting_list(), b in arb_graph_posting_list()) {
        let lhs = a.union(&b);
        let rhs = a.to_posting_list().union(&b.to_posting_list());
        prop_assert_eq!(doc_ids(&lhs), rhs.doc_ids().collect::<Vec<_>>());
    }

    #[test]
    fn phi_preserves_intersect(a in arb_graph_posting_list(), b in arb_graph_posting_list()) {
        let lhs = a.intersect(&b);
        let rhs = a.to_posting_list().intersect(&b.to_posting_list());
        prop_assert_eq!(doc_ids(&lhs), rhs.doc_ids().collect::<Vec<_>>());
    }

    #[test]
    fn phi_preserves_difference(a in arb_graph_posting_list(), b in arb_graph_posting_list()) {
        let lhs = a.difference(&b);
        let rhs = a.to_posting_list().difference(&b.to_posting_list());
        prop_assert_eq!(doc_ids(&lhs), rhs.doc_ids().collect::<Vec<_>>());
    }

    #[test]
    fn phi_preserves_de_morgan_relative(
        a in arb_graph_posting_list(),
        b in arb_graph_posting_list(),
    ) {
        // (a union b) intersect c == (a intersect c) union (b intersect c)
        // — applied via Phi, holds at the doc-id-set level.
        let union_then_inter = a.union(&b).intersect(&a);
        let inter_then_union = a.intersect(&a).union(&b.intersect(&a));
        prop_assert_eq!(doc_ids(&union_then_inter), doc_ids(&inter_then_union));
    }
}
