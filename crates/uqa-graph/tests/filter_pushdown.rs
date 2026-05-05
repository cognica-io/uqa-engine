//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Filter-pushdown property tests (Master Plan Section 2.4).
//!
//! For any graph `G`, label `L`, and vertex predicate `p`, evaluating
//! `VertexMatch(label=L)` and then post-filtering on `p` must produce
//! the same vertex set as `VertexMatch(label=L, predicate=p)` (the
//! "filter pushed into the pattern" form). This is the algebraic
//! invariant that lets the planner rewrite filters into pattern
//! constraints without changing semantics.

use std::collections::BTreeSet;

use proptest::prelude::*;
use uqa_core::{Value, Vertex, VertexId};
use uqa_graph::{GraphStore, MemoryGraphStore, VertexMatch, VertexPredicate};

const GRAPH: &str = "g";
const LABEL: &str = "person";

fn build_store(rows: &[(VertexId, &str, Option<i64>)]) -> MemoryGraphStore {
    let mut g = MemoryGraphStore::new();
    g.create_graph(GRAPH);
    for (id, label, salary) in rows {
        let mut v = Vertex::new(*id, *label);
        if let Some(s) = salary {
            v.properties.insert("salary".to_string(), Value::Int(*s));
        }
        g.add_vertex(v, GRAPH);
    }
    g
}

/// Pull just the matched vertex ids out of a `GraphPostingList`
/// (`doc_id` is the vertex id for `VertexMatch`).
fn matched_ids(g: &uqa_graph::GraphPostingList) -> BTreeSet<VertexId> {
    g.inner().doc_ids().collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    /// `VertexMatch(label=L) -> filter(salary == k)` equals
    /// `VertexMatch(label=L, predicate=salary_eq_k)`.
    #[test]
    fn property_eq_pushdown(
        rows in proptest::collection::vec((1u64..=20, prop_oneof!["person", "company"], proptest::option::of(0i64..200_000)), 0..=10),
        target_salary in 0i64..200_000,
    ) {
        let store = build_store(
            &rows
                .iter()
                .map(|(id, l, s)| (*id, l.as_str(), *s))
                .collect::<Vec<_>>(),
        );

        // Baseline: match label, then post-filter in Rust on salary == target_salary.
        let label_only = VertexMatch::new(GRAPH).label(LABEL).execute(&store);
        let post_filtered: BTreeSet<VertexId> = matched_ids(&label_only)
            .into_iter()
            .filter(|vid| {
                store
                    .get_vertex(*vid)
                    .and_then(|v| v.properties.get("salary").cloned())
                    .is_some_and(|v| matches!(v, Value::Int(s) if s == target_salary))
            })
            .collect();

        // Pushed-down: match label + predicate in one call.
        let pushed = VertexMatch::new(GRAPH)
            .label(LABEL)
            .predicate(VertexPredicate::PropertyEq {
                key: "salary".into(),
                value: Value::Int(target_salary),
            })
            .execute(&store);
        let pushed_set = matched_ids(&pushed);

        prop_assert_eq!(post_filtered, pushed_set);
    }

    /// `VertexMatch(label=L) -> filter(has salary)` equals
    /// `VertexMatch(label=L, predicate=PropertyExists("salary"))`.
    #[test]
    fn property_exists_pushdown(rows in proptest::collection::vec((1u64..=20, prop_oneof!["person", "company"], proptest::option::of(0i64..200_000)), 0..=10)) {
        let store = build_store(
            &rows
                .iter()
                .map(|(id, l, s)| (*id, l.as_str(), *s))
                .collect::<Vec<_>>(),
        );

        let label_only = VertexMatch::new(GRAPH).label(LABEL).execute(&store);
        let post_filtered: BTreeSet<VertexId> = matched_ids(&label_only)
            .into_iter()
            .filter(|vid| {
                store
                    .get_vertex(*vid)
                    .is_some_and(|v| v.properties.contains_key("salary"))
            })
            .collect();

        let pushed = VertexMatch::new(GRAPH)
            .label(LABEL)
            .predicate(VertexPredicate::PropertyExists("salary".into()))
            .execute(&store);
        let pushed_set = matched_ids(&pushed);

        prop_assert_eq!(post_filtered, pushed_set);
    }

    /// Conjunction of two pushdowns equals the AND-combined pushdown
    /// in one call: `VertexMatch(label=L, predicate=All([p1, p2]))`.
    #[test]
    fn conjunction_pushdown(rows in proptest::collection::vec((1u64..=20, prop_oneof!["person", "company"], proptest::option::of(0i64..200_000)), 0..=10), threshold in 0i64..200_000) {
        let store = build_store(
            &rows
                .iter()
                .map(|(id, l, s)| (*id, l.as_str(), *s))
                .collect::<Vec<_>>(),
        );

        // Reference: label only, post-filtered by both checks in Rust.
        let label_only = VertexMatch::new(GRAPH).label(LABEL).execute(&store);
        let two_step: BTreeSet<VertexId> = matched_ids(&label_only)
            .into_iter()
            .filter(|vid| {
                let Some(v) = store.get_vertex(*vid) else {
                    return false;
                };
                let has_salary = v.properties.contains_key("salary");
                let salary_match = matches!(
                    v.properties.get("salary"),
                    Some(Value::Int(s)) if *s >= threshold,
                );
                has_salary && salary_match
            })
            .collect();

        // One-shot conjunction pushed entirely into the pattern via `All`.
        // PropertyEq doesn't support range checks, so we use a Custom
        // predicate for the threshold.
        let store_for_pred = std::sync::Arc::new(());
        let _ = store_for_pred; // keep Arc usage explicit, even if Custom doesn't capture state
        let pushed = VertexMatch::new(GRAPH)
            .label(LABEL)
            .predicate(VertexPredicate::All(vec![
                VertexPredicate::PropertyExists("salary".into()),
                VertexPredicate::Custom(std::sync::Arc::new(move |v: &Vertex| {
                    matches!(
                        v.properties.get("salary"),
                        Some(Value::Int(s)) if *s >= threshold,
                    )
                })),
            ]))
            .execute(&store);
        let pushed_set = matched_ids(&pushed);

        prop_assert_eq!(two_step, pushed_set);
    }
}

#[test]
fn empty_graph_returns_empty() {
    let store = build_store(&[]);
    let m = VertexMatch::new(GRAPH).label(LABEL).execute(&store);
    assert!(m.inner().is_empty());
}
