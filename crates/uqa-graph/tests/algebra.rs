//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property tests for the lossless `Phi` encoding between
//! `GraphPostingList` and `PostingList` payload storage.
//!
//! For any composition of payload merges and exclusions on graph posting
//! lists, encoding through `Phi` and decoding back must
//! preserve the full encoded result. Boolean identities are asserted only on
//! document support.

use std::collections::BTreeMap;

use proptest::prelude::*;
use uqa_core::types::{GRAPH_PHI_EDGES_FIELD, GRAPH_PHI_FIELD, GRAPH_PHI_VERTICES_FIELD};
use uqa_core::{DocId, Payload, PostingEntry, PostingList, Value};
use uqa_graph::{GraphPayload, GraphPostingList};

fn arb_graph_posting_list() -> impl Strategy<Value = GraphPostingList> {
    prop::collection::btree_set(0u64..32, 0..10).prop_map(|ids| {
        let mut entries = Vec::new();
        let mut payloads: BTreeMap<DocId, GraphPayload> = BTreeMap::new();
        for id in ids {
            let mut fields = BTreeMap::from([("id".into(), Value::Int(id as i64))]);
            if id % 2 == 0 {
                fields.insert(GRAPH_PHI_FIELD.into(), Value::Str(format!("user-{id}")));
            }
            if id % 3 == 0 {
                fields.insert(GRAPH_PHI_VERTICES_FIELD.into(), Value::Null);
            }
            if id % 5 == 0 {
                fields.insert(
                    GRAPH_PHI_EDGES_FIELD.into(),
                    Value::Map(BTreeMap::from([("edge".into(), Value::Int(id as i64))])),
                );
            }
            entries.push(PostingEntry::new(
                id,
                Payload {
                    positions: vec![id as u32, id as u32 + 1],
                    score: id as f64 / 8.0 - 2.0,
                    fields,
                },
            ));
            if id % 4 != 0 {
                payloads.insert(
                    id,
                    GraphPayload {
                        subgraph_vertices: vec![id, u64::MAX - id, id],
                        subgraph_edges: vec![id * 10, id * 10],
                        graph_name: format!("graph-{id}"),
                        score_override: (id % 2 == 0).then_some(id as f64 + 0.25),
                    },
                );
            }
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
        prop_assert_eq!(g, back);
    }

    #[test]
    fn phi_preserves_union(a in arb_graph_posting_list(), b in arb_graph_posting_list()) {
        let lhs = a.merge_union(&b);
        let rhs = a.to_posting_list().merge_union(&b.to_posting_list());
        prop_assert_eq!(lhs.to_posting_list(), rhs);
    }

    #[test]
    fn phi_preserves_intersect(a in arb_graph_posting_list(), b in arb_graph_posting_list()) {
        let lhs = a.merge_intersection(&b);
        let rhs = a.to_posting_list().merge_intersection(&b.to_posting_list());
        prop_assert_eq!(lhs.to_posting_list(), rhs);
    }

    #[test]
    fn phi_preserves_difference(a in arb_graph_posting_list(), b in arb_graph_posting_list()) {
        let lhs = a.exclude(&b);
        let rhs = a.to_posting_list().exclude(&b.to_posting_list());
        prop_assert_eq!(lhs.to_posting_list(), rhs);
    }

    #[test]
    fn phi_preserves_de_morgan_relative(
        a in arb_graph_posting_list(),
        b in arb_graph_posting_list(),
    ) {
        // (a union b) intersect c == (a intersect c) union (b intersect c)
        // — applied via Phi, holds at the doc-id-set level.
        let union_then_inter = a.merge_union(&b).merge_intersection(&a);
        let inter_then_union = a
            .merge_intersection(&a)
            .merge_union(&b.merge_intersection(&a));
        prop_assert_eq!(doc_ids(&union_then_inter), doc_ids(&inter_then_union));
    }
}
