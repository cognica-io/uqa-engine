//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property tests for the lossless graph codec and the explicit graph-result
//! merge contract. Boolean identities are asserted only on document support;
//! subgraph metadata is checked against an independently computed set merge.

use std::collections::{BTreeMap, BTreeSet};

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
        GraphPostingList::try_from_parts(PostingList::from_sorted_unchecked(entries), payloads)
            .expect("generated graph payloads are within posting support")
    })
}

fn doc_ids(g: &GraphPostingList) -> Vec<DocId> {
    g.inner().doc_ids().collect()
}

fn set_union(left: &[u64], right: &[u64]) -> Vec<u64> {
    left.iter()
        .chain(right)
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn set_intersection(left: &[u64], right: &[u64]) -> Vec<u64> {
    let right: BTreeSet<_> = right.iter().copied().collect();
    left.iter()
        .copied()
        .filter(|value| right.contains(value))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn assert_merged_graph_payload(
    merged: &GraphPostingList,
    left: &GraphPostingList,
    right: &GraphPostingList,
    doc_id: DocId,
    merge: fn(&[u64], &[u64]) -> Vec<u64>,
) {
    match (
        left.get_graph_payload(doc_id),
        right.get_graph_payload(doc_id),
    ) {
        (None, None) => assert!(merged.get_graph_payload(doc_id).is_none()),
        (Some(expected), None) | (None, Some(expected)) => {
            assert_eq!(merged.get_graph_payload(doc_id), Some(expected));
        }
        (Some(left), Some(right)) => {
            let actual = merged.get_graph_payload(doc_id).unwrap();
            assert_eq!(
                actual.subgraph_vertices,
                merge(&left.subgraph_vertices, &right.subgraph_vertices)
            );
            assert_eq!(
                actual.subgraph_edges,
                merge(&left.subgraph_edges, &right.subgraph_edges)
            );
            assert_eq!(actual.graph_name, left.graph_name);
        }
    }
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
    fn graph_union_has_support_union_and_subgraph_set_union(
        a in arb_graph_posting_list(),
        b in arb_graph_posting_list(),
    ) {
        let merged = a.merge_union(&b).unwrap();
        let expected_support = a
            .inner()
            .doc_ids()
            .chain(b.inner().doc_ids())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        prop_assert_eq!(doc_ids(&merged), expected_support);
        for doc_id in merged.inner().doc_ids() {
            assert_merged_graph_payload(&merged, &a, &b, doc_id, set_union);
        }
    }

    #[test]
    fn graph_intersection_has_support_intersection_and_subgraph_set_intersection(
        a in arb_graph_posting_list(),
        b in arb_graph_posting_list(),
    ) {
        let merged = a.merge_intersection(&b).unwrap();
        let right_support = b.inner().doc_ids().collect::<BTreeSet<_>>();
        let expected_support = a
            .inner()
            .doc_ids()
            .filter(|doc_id| right_support.contains(doc_id))
            .collect::<Vec<_>>();
        prop_assert_eq!(doc_ids(&merged), expected_support);
        for doc_id in merged.inner().doc_ids() {
            assert_merged_graph_payload(&merged, &a, &b, doc_id, set_intersection);
        }
    }

    #[test]
    fn difference_preserves_surviving_left_values(
        a in arb_graph_posting_list(),
        b in arb_graph_posting_list(),
    ) {
        let difference = a.exclude(&b);
        let right_support = b.inner().doc_ids().collect::<BTreeSet<_>>();
        let expected_support = a
            .inner()
            .doc_ids()
            .filter(|doc_id| !right_support.contains(doc_id))
            .collect::<Vec<_>>();
        prop_assert_eq!(doc_ids(&difference), expected_support);
        for doc_id in difference.inner().doc_ids() {
            prop_assert_eq!(difference.inner().get_entry(doc_id), a.inner().get_entry(doc_id));
            prop_assert_eq!(difference.get_graph_payload(doc_id), a.get_graph_payload(doc_id));
        }
    }

    #[test]
    fn support_satisfies_relative_distributivity(
        a in arb_graph_posting_list(),
        b in arb_graph_posting_list(),
    ) {
        // (a union b) intersect a == (a intersect a) union (b intersect a)
        // holds at the document-support level only.
        let union_then_inter = a.merge_union(&b).unwrap().merge_intersection(&a).unwrap();
        let inter_then_union = a
            .merge_intersection(&a)
            .unwrap()
            .merge_union(&b.merge_intersection(&a).unwrap())
            .unwrap();
        prop_assert_eq!(doc_ids(&union_then_inter), doc_ids(&inter_then_union));
    }
}
