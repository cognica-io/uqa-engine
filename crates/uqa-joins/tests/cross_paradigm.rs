//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integration tests for cross-paradigm join operators.

use std::collections::BTreeMap;

use uqa_core::{Edge, Payload, PostingEntry, Value, Vertex};
use uqa_graph::{GraphStore, MemoryGraphStore};
use uqa_joins::{
    CrossParadigmJoin, GraphJoin, HybridJoin, TextSimilarityJoin, VectorSimilarityJoin,
};

fn entry(id: u64, fields: BTreeMap<String, Value>, score: f64) -> PostingEntry {
    PostingEntry::new(
        id,
        Payload {
            positions: Vec::new(),
            score,
            fields,
        },
    )
}

fn fields(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

#[test]
fn text_similarity_join_emits_pairs_above_threshold() {
    let left = vec![
        entry(
            1,
            fields(&[("title", Value::Str("rust language".into()))]),
            0.0,
        ),
        entry(
            2,
            fields(&[("title", Value::Str("python ecosystem".into()))]),
            0.0,
        ),
    ];
    let right = vec![
        entry(
            10,
            fields(&[("title", Value::Str("rust language guide".into()))]),
            0.0,
        ),
        entry(
            11,
            fields(&[("title", Value::Str("python web".into()))]),
            0.0,
        ),
    ];
    let result = TextSimilarityJoin::new(&left, &right, "title", "title")
        .threshold(0.4)
        .execute()
        .unwrap();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert!(pairs.contains(&(1, 10)));
}

#[test]
fn vector_similarity_join_filters_by_cosine() {
    let v_a = Value::List(vec![Value::Float(1.0), Value::Float(0.0)]);
    let v_b = Value::List(vec![Value::Float(0.9), Value::Float(0.1)]);
    let v_c = Value::List(vec![Value::Float(0.0), Value::Float(1.0)]);
    let left = vec![entry(1, fields(&[("emb", v_a.clone())]), 0.0)];
    let right = vec![
        entry(10, fields(&[("emb", v_b)]), 0.0),
        entry(11, fields(&[("emb", v_c)]), 0.0),
    ];
    let result = VectorSimilarityJoin::new(&left, &right, "emb", "emb")
        .threshold(0.5)
        .execute()
        .unwrap();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert_eq!(pairs, vec![(1, 10)]);
}

#[test]
fn hybrid_join_filters_by_structured_field_and_cosine() {
    let v_a = Value::List(vec![Value::Float(1.0), Value::Float(0.0)]);
    let v_b = Value::List(vec![Value::Float(0.95), Value::Float(0.0)]);
    let left = vec![
        entry(
            1,
            fields(&[("dept", Value::Int(10)), ("emb", v_a.clone())]),
            0.0,
        ),
        entry(
            2,
            fields(&[("dept", Value::Int(20)), ("emb", v_a.clone())]),
            0.0,
        ),
    ];
    let right = vec![
        entry(
            10,
            fields(&[("dept", Value::Int(10)), ("emb", v_b.clone())]),
            0.0,
        ),
        entry(11, fields(&[("dept", Value::Int(30)), ("emb", v_b)]), 0.0),
    ];
    let result = HybridJoin::new(&left, &right, "dept", "emb")
        .threshold(0.5)
        .execute()
        .unwrap();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert_eq!(pairs, vec![(1, 10)]);
}

#[test]
fn graph_join_emits_pairs_connected_by_edge() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g").unwrap();
    for v in 1..=3 {
        g.add_vertex(Vertex::new(v, "n"), "g").unwrap();
    }
    g.add_edge(Edge::new(10, 1, 2, "knows"), "g").unwrap();
    g.add_edge(Edge::new(11, 1, 3, "likes"), "g").unwrap();

    let left = vec![entry(1, BTreeMap::new(), 0.5)];
    let right = vec![
        entry(2, BTreeMap::new(), 0.4),
        entry(3, BTreeMap::new(), 0.3),
    ];
    let result = GraphJoin::new(&left, &right, &g, "g")
        .label("knows")
        .execute()
        .unwrap();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert_eq!(pairs, vec![(1, 2)]);
    if let Some(Value::Float(s)) = result.entries()[0].payload.fields.get("_score") {
        assert!((s - 0.9).abs() < 1e-9);
    } else {
        panic!("missing _score");
    }
}

#[test]
fn cross_paradigm_join_bridges_vertex_property_to_doc_field() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g").unwrap();
    let mut alice = Vertex::new(1, "Person");
    alice
        .properties
        .insert("email".into(), Value::Str("alice@example.com".into()));
    g.add_vertex(alice, "g").unwrap();

    let left = vec![entry(1, BTreeMap::new(), 0.0)];
    let right = vec![
        entry(
            100,
            fields(&[("email", Value::Str("alice@example.com".into()))]),
            0.0,
        ),
        entry(
            101,
            fields(&[("email", Value::Str("bob@example.com".into()))]),
            0.0,
        ),
    ];
    let result = CrossParadigmJoin::new(&left, &right, &g, "email", "email")
        .execute()
        .unwrap();
    let pairs: Vec<(u64, u64)> = result
        .entries()
        .iter()
        .map(|e| (e.doc_ids[0], e.doc_ids[1]))
        .collect();
    assert_eq!(pairs, vec![(1, 100)]);
}

#[test]
fn vector_similarity_join_rejects_malformed_and_non_finite_vectors() {
    let valid = vec![entry(
        1,
        fields(&[(
            "emb",
            Value::List(vec![Value::Float(1.0), Value::Float(0.0)]),
        )]),
        0.0,
    )];
    for bad in [
        Value::Str("not a vector".into()),
        Value::List(vec![Value::Float(f64::NAN)]),
        Value::List(vec![Value::Int((1_i64 << 53) + 1)]),
        Value::List(Vec::new()),
    ] {
        let invalid = vec![entry(2, fields(&[("emb", bad)]), 0.0)];
        assert!(VectorSimilarityJoin::new(&valid, &invalid, "emb", "emb")
            .execute()
            .is_err());
    }

    let wrong_dimension = vec![entry(
        3,
        fields(&[("emb", Value::List(vec![Value::Float(1.0)]))]),
        0.0,
    )];
    assert!(
        VectorSimilarityJoin::new(&valid, &wrong_dimension, "emb", "emb")
            .execute()
            .is_err()
    );
}

#[test]
fn hybrid_join_rejects_zero_norm_vector() {
    let left = vec![entry(
        1,
        fields(&[
            ("dept", Value::Int(10)),
            (
                "emb",
                Value::List(vec![Value::Float(0.0), Value::Float(0.0)]),
            ),
        ]),
        0.0,
    )];
    let right = vec![entry(
        2,
        fields(&[
            ("dept", Value::Int(10)),
            (
                "emb",
                Value::List(vec![Value::Float(1.0), Value::Float(0.0)]),
            ),
        ]),
        0.0,
    )];
    assert!(HybridJoin::new(&left, &right, "dept", "emb")
        .execute()
        .is_err());
}

#[test]
fn cross_paradigm_join_rejects_missing_vertex_and_non_finite_scores() {
    let mut g = MemoryGraphStore::new();
    g.create_graph("g").unwrap();
    let right = vec![entry(
        10,
        fields(&[("email", Value::Str("alice@example.com".into()))]),
        0.0,
    )];
    let missing = vec![entry(999, BTreeMap::new(), 0.0)];
    assert!(
        CrossParadigmJoin::new(&missing, &right, &g, "email", "email")
            .execute()
            .is_err()
    );

    let mut alice = Vertex::new(1, "Person");
    alice
        .properties
        .insert("email".into(), Value::Str("alice@example.com".into()));
    g.add_vertex(alice, "g").unwrap();
    let non_finite = vec![entry(1, BTreeMap::new(), f64::INFINITY)];
    assert!(
        CrossParadigmJoin::new(&non_finite, &right, &g, "email", "email")
            .execute()
            .is_err()
    );

    let overflow_left = vec![entry(1, BTreeMap::new(), f64::MAX)];
    let overflow_right = vec![entry(
        10,
        fields(&[("email", Value::Str("alice@example.com".into()))]),
        f64::MAX,
    )];
    assert!(
        CrossParadigmJoin::new(&overflow_left, &overflow_right, &g, "email", "email")
            .execute()
            .is_err()
    );
}

#[test]
fn right_input_preparation_scales_with_rows_for_text_vector_and_hybrid_joins() {
    fn allocation_counts(count: u64) -> [u64; 3] {
        let postings = |text: &str, vector: [f64; 2]| {
            (1..=count)
                .map(|id| {
                    entry(
                        id,
                        fields(&[
                            ("text", Value::Str(text.into())),
                            ("group", Value::Int(1)),
                            ("vec", Value::List(vector.map(Value::Float).to_vec())),
                        ]),
                        0.0,
                    )
                })
                .collect::<Vec<_>>()
        };
        let left = postings("espresso coffee", [1.0, 0.0]);
        let right = postings("train station", [0.0, 1.0]);
        [
            allocation_counter::measure(|| {
                assert!(TextSimilarityJoin::new(&left, &right, "text", "text")
                    .threshold(1.0)
                    .execute()
                    .unwrap()
                    .entries()
                    .is_empty());
            })
            .count_total,
            allocation_counter::measure(|| {
                assert!(VectorSimilarityJoin::new(&left, &right, "vec", "vec")
                    .threshold(1.0)
                    .execute()
                    .unwrap()
                    .entries()
                    .is_empty());
            })
            .count_total,
            allocation_counter::measure(|| {
                assert!(HybridJoin::new(&left, &right, "group", "vec")
                    .threshold(1.0)
                    .execute()
                    .unwrap()
                    .entries()
                    .is_empty());
            })
            .count_total,
        ]
    }
    let small = allocation_counts(4);
    // Preparing the four-row text join also bounds the fixed cost of the standard analyzer.
    assert!(
        small[0] <= 384,
        "four-row text join took {} allocations",
        small[0]
    );
    let large = allocation_counts(32);
    for (small, large) in small.into_iter().zip(large) {
        assert!(
            small > 0 && large <= 8 * small,
            "allocations with 4 rows: {small}, with 32 rows: {large}"
        );
    }
}

#[test]
fn vector_errors_follow_the_first_reachable_pair() {
    let zero = Value::List(vec![Value::Int(0)]);
    let pair = Value::List(vec![Value::Int(1), Value::Int(1)]);
    let left = [entry(
        1,
        fields(&[("vec", zero), ("group", Value::Int(1))]),
        0.0,
    )];
    let right = [
        entry(2, fields(&[("vec", pair), ("group", Value::Int(1))]), 0.0),
        entry(
            3,
            fields(&[("vec", Value::Bool(true)), ("group", Value::Int(2))]),
            0.0,
        ),
    ];
    let error = VectorSimilarityJoin::new(&left, &right, "vec", "vec")
        .execute()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("same non-zero dimension (1 != 2)"),
        "{error}"
    );
    let hybrid = HybridJoin::new(&left, &right, "group", "vec")
        .execute()
        .unwrap_err();
    assert_eq!(error, hybrid.to_string());
    assert!(HybridJoin::new(&left, &right[1..], "group", "vec")
        .execute()
        .unwrap()
        .entries()
        .is_empty());
    assert!(VectorSimilarityJoin::new(&[], &right, "vec", "vec")
        .execute()
        .unwrap()
        .entries()
        .is_empty());
}

#[test]
fn prepared_vector_pairs_preserve_scaled_cosine_score_bits() {
    let cases: &[([f64; 3], [f64; 3], u64)] = &[
        ([1.0, 3.0, -4.0], [5.0, -2.0, 7.0], 0xbfe4_9b61_f4f9_7415),
        (
            [f64::MAX, f64::MAX / 4.0, -f64::MAX],
            [f64::MAX / 8.0, f64::MAX, f64::MAX / 2.0],
            0xbfb3_ce61_1505_0364,
        ),
        (
            [f64::from_bits(1), f64::from_bits(2), 0.0],
            [f64::from_bits(3), 0.0, f64::from_bits(1)],
            0x3fdb_2724_7aff_148f,
        ),
    ];
    for &(a, b, expected) in cases {
        let posting = |id, v: [f64; 3]| {
            entry(
                id,
                fields(&[
                    ("v", Value::List(v.map(Value::Float).to_vec())),
                    ("g", Value::Int(1)),
                ]),
                0.0,
            )
        };
        let (left, right) = ([posting(1, a)], [posting(2, b)]);
        for result in [
            VectorSimilarityJoin::new(&left, &right, "v", "v")
                .threshold(-1.0)
                .execute()
                .unwrap(),
            HybridJoin::new(&left, &right, "g", "v")
                .threshold(-1.0)
                .execute()
                .unwrap(),
        ] {
            let Value::Float(score) = result.entries()[0].payload.fields["_score"] else {
                panic!("expected score")
            };
            assert_eq!(score.to_bits(), expected);
        }
    }
}
