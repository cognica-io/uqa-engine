use uqa_core::Payload;

use super::*;

fn entry(doc_id: DocId, score: f64) -> PostingEntry {
    PostingEntry::new(doc_id, Payload::with_score(score))
}

fn fixture(doc_id: DocId, vertices: Vec<VertexId>, edges: Vec<EdgeId>) -> GraphPostingList {
    let mut gpl = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![entry(doc_id, 1.0)]),
        BTreeMap::new(),
    )
    .unwrap();
    gpl.try_set_graph_payload(
        doc_id,
        GraphPayload {
            subgraph_vertices: vertices,
            subgraph_edges: edges,
            ..GraphPayload::default()
        },
    )
    .unwrap();
    gpl
}

#[test]
fn graph_payload_support_is_enforced_by_construction_and_mutation() {
    let inner = PostingList::from_sorted_unchecked(vec![entry(1, 1.0)]);
    let error = GraphPostingList::try_from_parts(
        inner.clone(),
        BTreeMap::from([(2, GraphPayload::default())]),
    )
    .unwrap_err();
    assert_eq!(
        error,
        GraphPostingListError::PayloadOutsideSupport { doc_id: 2 }
    );

    let mut graph = GraphPostingList::try_from_parts(inner, BTreeMap::new()).unwrap();
    let error = graph
        .try_set_graph_payload(2, GraphPayload::default())
        .unwrap_err();
    assert_eq!(
        error,
        GraphPostingListError::PayloadOutsideSupport { doc_id: 2 }
    );
    assert!(graph.get_graph_payload(2).is_none());
}

#[test]
fn round_trip_preserves_the_complete_payload_and_reserved_collisions() {
    let colliding_envelope = GraphPhiEnvelope {
        base_score: -11.0,
        graph_payload: None,
        score_override: None,
        original_reserved: Some(Value::Null),
        original_vertices: None,
        original_edges: None,
    }
    .encode();
    let entries = vec![
        PostingEntry::new(
            7,
            Payload {
                positions: vec![1, 3, 8],
                score: 1.25,
                fields: BTreeMap::from([
                    ("ordinary".into(), Value::Str("kept".into())),
                    (GRAPH_PHI_FIELD.into(), colliding_envelope),
                    (GRAPH_PHI_VERTICES_FIELD.into(), Value::Null),
                    (
                        GRAPH_PHI_EDGES_FIELD.into(),
                        Value::Map(BTreeMap::from([("application".into(), Value::Bool(true))])),
                    ),
                ]),
            },
        ),
        PostingEntry::new(
            8,
            Payload {
                positions: vec![2],
                score: -0.0,
                fields: BTreeMap::new(),
            },
        ),
        PostingEntry::new(
            9,
            Payload {
                positions: Vec::new(),
                score: 3.0,
                fields: BTreeMap::from([(
                    GRAPH_PHI_VERTICES_FIELD.into(),
                    Value::Str("not graph metadata".into()),
                )]),
            },
        ),
    ];
    let mut graph_payloads = BTreeMap::new();
    graph_payloads.insert(
        7,
        GraphPayload {
            subgraph_vertices: vec![u64::MAX, 3, 3],
            subgraph_edges: vec![9, u64::MAX - 1],
            graph_name: "knowledge.prod".into(),
            score_override: Some(-7.5),
        },
    );
    graph_payloads.insert(
        8,
        GraphPayload {
            subgraph_vertices: vec![8],
            subgraph_edges: Vec::new(),
            graph_name: "no-override".into(),
            score_override: None,
        },
    );
    let gpl = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(entries),
        graph_payloads,
    )
    .unwrap();

    let encoded = gpl.to_posting_list();
    assert_eq!(encoded.get_entry(7).unwrap().payload.score, -7.5);
    assert_eq!(
        encoded
            .get_entry(7)
            .unwrap()
            .payload
            .fields
            .get(GRAPH_PHI_VERTICES_FIELD),
        Some(&Value::List(vec![
            Value::Bytes(u64::MAX.to_be_bytes().to_vec()),
            Value::Bytes(3_u64.to_be_bytes().to_vec()),
            Value::Bytes(3_u64.to_be_bytes().to_vec()),
        ]))
    );

    let restored = GraphPostingList::from_posting_list(&encoded);
    assert_eq!(restored, gpl);
    assert_eq!(
        restored
            .inner()
            .get_entry(8)
            .unwrap()
            .payload
            .score
            .to_bits(),
        (-0.0_f64).to_bits()
    );
    assert!(restored.get_graph_payload(9).is_none());
}

#[test]
fn round_trip_preserves_ids_above_signed_bigint_range() {
    let gpl = fixture(7, vec![u64::MAX], vec![u64::MAX - 1]);
    let restored = GraphPostingList::from_posting_list(&gpl.to_posting_list());
    let payload = restored.get_graph_payload(7).unwrap();
    assert_eq!(payload.subgraph_vertices, vec![u64::MAX]);
    assert_eq!(payload.subgraph_edges, vec![u64::MAX - 1]);
}

#[test]
fn union_combines_disjoint_support() {
    let a = fixture(1, vec![1], vec![]);
    let b = fixture(2, vec![2], vec![]);
    let merged = a.merge_union(&b).unwrap();
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
fn default_overlap_policies_apply_set_union_and_intersection() {
    let a = fixture(1, vec![3, 1, 3], vec![10, 20]);
    let b = fixture(1, vec![2, 3], vec![20, 30]);

    let merged = a.merge_union(&b).unwrap();
    let ids: Vec<DocId> = merged.inner().doc_ids().collect();
    assert_eq!(ids, vec![1]);
    let payload = merged.get_graph_payload(1).unwrap();
    assert_eq!(payload.subgraph_vertices, vec![1, 2, 3]);
    assert_eq!(payload.subgraph_edges, vec![10, 20, 30]);

    let merged = a.merge_intersection(&b).unwrap();
    let payload = merged.get_graph_payload(1).unwrap();
    assert_eq!(payload.subgraph_vertices, vec![3]);
    assert_eq!(payload.subgraph_edges, vec![20]);
}

#[test]
fn graph_name_conflicts_require_an_explicit_precedence_policy() {
    let mut left = fixture(1, vec![1], vec![10]);
    left.try_set_graph_payload(
        1,
        GraphPayload {
            subgraph_vertices: vec![1],
            subgraph_edges: vec![10],
            graph_name: "left".into(),
            score_override: None,
        },
    )
    .unwrap();
    let mut right = fixture(1, vec![2], vec![20]);
    right
        .try_set_graph_payload(
            1,
            GraphPayload {
                subgraph_vertices: vec![2],
                subgraph_edges: vec![20],
                graph_name: "right".into(),
                score_override: None,
            },
        )
        .unwrap();

    assert_eq!(
        left.merge_union(&right).unwrap_err(),
        GraphPostingListError::ConflictingGraphNames {
            doc_id: 1,
            left: "left".into(),
            right: "right".into(),
        }
    );
    let merged = left
        .merge_union_with(&right, SubgraphMergePolicy::PreferRight)
        .unwrap();
    assert_eq!(
        merged.get_graph_payload(1),
        Some(&GraphPayload {
            subgraph_vertices: vec![2],
            subgraph_edges: vec![20],
            graph_name: "right".into(),
            score_override: None,
        })
    );
}

#[test]
fn generic_posting_merge_retains_its_right_precedence_codec_contract() {
    let mut a = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            1,
            Payload {
                positions: vec![1, 3],
                score: 1.0,
                fields: BTreeMap::from([
                    ("side".into(), Value::Str("left".into())),
                    (GRAPH_PHI_FIELD.into(), Value::Str("left carrier".into())),
                ]),
            },
        )]),
        BTreeMap::new(),
    )
    .unwrap();
    a.try_set_graph_payload(
        1,
        GraphPayload {
            subgraph_vertices: vec![1],
            subgraph_edges: vec![10],
            graph_name: "left".into(),
            score_override: None,
        },
    )
    .unwrap();
    let mut b = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            1,
            Payload {
                positions: vec![2, 3],
                score: 2.0,
                fields: BTreeMap::from([
                    ("side".into(), Value::Str("right".into())),
                    (
                        GRAPH_PHI_FIELD.into(),
                        Value::Map(BTreeMap::from([("rhs".into(), Value::Int(1))])),
                    ),
                ]),
            },
        )]),
        BTreeMap::new(),
    )
    .unwrap();
    b.try_set_graph_payload(
        1,
        GraphPayload {
            subgraph_vertices: vec![2],
            subgraph_edges: vec![20],
            graph_name: "right".into(),
            score_override: Some(5.0),
        },
    )
    .unwrap();

    for encoded in [
        a.to_posting_list().merge_union(&b.to_posting_list()),
        a.to_posting_list().merge_intersection(&b.to_posting_list()),
    ] {
        let decoded = GraphPostingList::from_posting_list(&encoded);
        let entry = decoded.inner().get_entry(1).unwrap();
        assert_eq!(entry.payload.positions, vec![1, 2, 3]);
        assert_eq!(entry.payload.score, 3.0);
        assert_eq!(
            entry.payload.fields.get("side"),
            Some(&Value::Str("right".into()))
        );
        assert_eq!(
            entry.payload.fields.get(GRAPH_PHI_FIELD),
            Some(&Value::Map(BTreeMap::from([("rhs".into(), Value::Int(1))])))
        );
        assert_eq!(
            decoded.get_graph_payload(1),
            Some(&GraphPayload {
                subgraph_vertices: vec![2],
                subgraph_edges: vec![20],
                graph_name: "right".into(),
                score_override: Some(6.0),
            })
        );
        assert_eq!(decoded.to_posting_list(), encoded);
    }
}

#[test]
fn payload_overlap_keeps_none_override_when_both_inputs_use_base_scores() {
    let a = fixture(1, vec![1], vec![10]);
    let mut b = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![entry(1, 2.0)]),
        BTreeMap::new(),
    )
    .unwrap();
    b.try_set_graph_payload(
        1,
        GraphPayload {
            subgraph_vertices: vec![2],
            subgraph_edges: vec![20],
            ..GraphPayload::default()
        },
    )
    .unwrap();
    let merged = a.merge_union(&b).unwrap();
    assert_eq!(merged.inner().get_entry(1).unwrap().payload.score, 3.0);
    assert_eq!(merged.get_graph_payload(1).unwrap().score_override, None);
    assert_eq!(
        merged.get_graph_payload(1).unwrap().subgraph_vertices,
        vec![1, 2]
    );
    assert_eq!(
        merged.get_graph_payload(1).unwrap().subgraph_edges,
        vec![10, 20]
    );
    assert_eq!(
        merged.to_posting_list().get_entry(1).unwrap().payload.score,
        3.0
    );
}

#[test]
fn payload_overlap_with_plain_payload_keeps_graph_and_effective_score() {
    let mut graph = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![entry(1, 1.0)]),
        BTreeMap::new(),
    )
    .unwrap();
    graph
        .try_set_graph_payload(
            1,
            GraphPayload {
                subgraph_vertices: vec![1],
                subgraph_edges: vec![7],
                graph_name: "left".into(),
                score_override: Some(4.0),
            },
        )
        .unwrap();
    let plain = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![entry(1, 2.0)]),
        BTreeMap::new(),
    )
    .unwrap();

    for merged in [graph.merge_union(&plain), plain.merge_union(&graph)] {
        let merged = merged.unwrap();
        assert_eq!(merged.inner().get_entry(1).unwrap().payload.score, 3.0);
        assert_eq!(
            merged.get_graph_payload(1),
            Some(&GraphPayload {
                subgraph_vertices: vec![1],
                subgraph_edges: vec![7],
                graph_name: "left".into(),
                score_override: Some(6.0),
            })
        );
        assert_eq!(
            merged.to_posting_list().get_entry(1).unwrap().payload.score,
            6.0
        );
    }
}

#[test]
fn round_trip_preserves_nan_score_bits() {
    let base_score = f64::from_bits(0x7ff8_0000_0000_0123);
    let override_score = f64::from_bits(0x7ff8_0000_0000_0456);
    let mut graph = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![entry(1, base_score)]),
        BTreeMap::new(),
    )
    .unwrap();
    graph
        .try_set_graph_payload(
            1,
            GraphPayload {
                graph_name: "nan".into(),
                score_override: Some(override_score),
                ..GraphPayload::default()
            },
        )
        .unwrap();
    let restored = GraphPostingList::from_posting_list(&graph.to_posting_list());
    assert_eq!(
        restored
            .inner()
            .get_entry(1)
            .unwrap()
            .payload
            .score
            .to_bits(),
        base_score.to_bits()
    );
    assert_eq!(
        restored
            .get_graph_payload(1)
            .unwrap()
            .score_override
            .unwrap()
            .to_bits(),
        override_score.to_bits()
    );
}

#[test]
fn malformed_recognized_envelope_is_preserved_without_legacy_fallback() {
    let malformed = Value::Map(BTreeMap::from([
        ("magic".into(), Value::Str("uqa.graph.phi".into())),
        ("version".into(), Value::Int(999)),
    ]));
    let posting = PostingList::from_sorted_unchecked(vec![PostingEntry::new(
        4,
        Payload {
            positions: vec![1],
            score: 2.0,
            fields: BTreeMap::from([
                (GRAPH_PHI_FIELD.into(), malformed),
                (
                    GRAPH_PHI_VERTICES_FIELD.into(),
                    Value::List(vec![Value::Int(4)]),
                ),
            ]),
        },
    )]);
    let decoded = GraphPostingList::from_posting_list(&posting);
    assert_eq!(decoded.inner(), &posting);
    assert!(decoded.get_graph_payload(4).is_none());
}

#[test]
fn legacy_encoding_remains_readable() {
    let posting = PostingList::from_sorted_unchecked(vec![PostingEntry::new(
        5,
        Payload {
            positions: vec![7],
            score: 9.0,
            fields: BTreeMap::from([
                (
                    GRAPH_PHI_VERTICES_FIELD.into(),
                    Value::List(vec![
                        Value::Int(3),
                        Value::Bytes(u64::MAX.to_be_bytes().to_vec()),
                    ]),
                ),
                (
                    GRAPH_PHI_EDGES_FIELD.into(),
                    Value::List(vec![Value::Int(8)]),
                ),
            ]),
        },
    )]);
    let decoded = GraphPostingList::from_posting_list(&posting);
    assert_eq!(
        decoded.get_graph_payload(5),
        Some(&GraphPayload {
            subgraph_vertices: vec![3, u64::MAX],
            subgraph_edges: vec![8],
            graph_name: String::new(),
            score_override: Some(9.0),
        })
    );
    assert!(decoded
        .inner()
        .get_entry(5)
        .unwrap()
        .payload
        .fields
        .is_empty());
}

#[test]
fn difference_removes_other_support() {
    let a = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![entry(1, 1.0), entry(2, 1.0)]),
        BTreeMap::new(),
    )
    .unwrap();
    let b = fixture(2, vec![], vec![]);
    let diff = a.exclude(&b);
    let ids: Vec<DocId> = diff.inner().doc_ids().collect();
    assert_eq!(ids, vec![1]);
}

#[test]
fn difference_preserves_the_surviving_left_payload_exactly() {
    let mut left = GraphPostingList::try_from_parts(
        PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            1,
            Payload {
                positions: vec![2, 9],
                score: -4.0,
                fields: BTreeMap::from([(
                    GRAPH_PHI_EDGES_FIELD.into(),
                    Value::Str("application value".into()),
                )]),
            },
        )]),
        BTreeMap::new(),
    )
    .unwrap();
    left.try_set_graph_payload(
        1,
        GraphPayload {
            subgraph_vertices: vec![1, 1],
            subgraph_edges: vec![u64::MAX],
            graph_name: "left".into(),
            score_override: Some(12.0),
        },
    )
    .unwrap();
    let right = fixture(2, vec![2], vec![2]);
    assert_eq!(left.exclude(&right), left);
}
