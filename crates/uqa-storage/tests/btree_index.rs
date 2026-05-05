//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `BTreeIndex` predicate-scan correctness.
//!
//! Pins:
//! - `Equals(v)` returns exactly the doc ids whose stored value equals
//!   `v`,
//! - `Between(lo, hi)` returns every doc whose value lies in
//!   `[lo, hi]` (inclusive on both ends),
//! - `GreaterThan(v)` and `LessThanOrEqual(v)` partition the indexed
//!   doc set without overlap,
//! - `InSet([v1, v2, ...])` equals the union of `Equals(v_i)` for
//!   each member,
//! - insert + remove on the same `(doc_id, value)` leaves the doc
//!   absent from any subsequent scan.

use std::collections::BTreeSet;

use proptest::prelude::*;
use uqa_core::{DocId, Predicate, Value};
use uqa_storage::BTreeIndex;

/// Build a `BTreeIndex` from `(doc_id, value)` pairs and return both
/// the index and a parallel oracle map for cross-checking.
fn build_index(pairs: &[(DocId, i64)]) -> (BTreeIndex, Vec<(DocId, i64)>) {
    let mut idx = BTreeIndex::new("v");
    for (doc, val) in pairs {
        idx.insert(*doc, Value::Int(*val));
    }
    (idx, pairs.to_vec())
}

fn ids(pl: &uqa_core::PostingList) -> BTreeSet<DocId> {
    pl.doc_ids().collect()
}

/// Strategy: a small set of `(doc_id, value)` pairs where `doc_ids`
/// are unique within the set.
fn arb_pairs() -> impl Strategy<Value = Vec<(DocId, i64)>> {
    proptest::collection::vec((1u64..=20, -50i64..=50), 0..=12).prop_map(|raw| {
        let mut seen: std::collections::BTreeMap<DocId, i64> = std::collections::BTreeMap::new();
        for (doc, val) in raw {
            seen.insert(doc, val);
        }
        seen.into_iter().collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        ..ProptestConfig::default()
    })]

    /// `Equals(v)` returns exactly the doc ids whose stored value is `v`.
    #[test]
    fn equals_matches_oracle(pairs in arb_pairs(), needle in -50i64..=50) {
        let (idx, oracle) = build_index(&pairs);
        let observed = ids(&idx.scan(&Predicate::Equals(Value::Int(needle))));
        let expected: BTreeSet<DocId> = oracle
            .iter()
            .filter(|(_, v)| *v == needle)
            .map(|(d, _)| *d)
            .collect();
        prop_assert_eq!(observed, expected);
    }

    /// `Between(lo, hi)` returns every doc with value in `[lo, hi]`.
    #[test]
    fn between_inclusive_on_both_ends(pairs in arb_pairs(), lo in -50i64..=50, hi in -50i64..=50) {
        let (low, high) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        let (idx, oracle) = build_index(&pairs);
        let observed = ids(&idx.scan(&Predicate::Between {
            low: Value::Int(low),
            high: Value::Int(high),
        }));
        let expected: BTreeSet<DocId> = oracle
            .iter()
            .filter(|(_, v)| *v >= low && *v <= high)
            .map(|(d, _)| *d)
            .collect();
        prop_assert_eq!(observed, expected);
    }

    /// `GreaterThan(v)` and `LessThanOrEqual(v)` partition the indexed
    /// doc set: union covers everything inserted, intersection is empty.
    #[test]
    fn gt_and_lte_partition(pairs in arb_pairs(), pivot in -50i64..=50) {
        let (idx, oracle) = build_index(&pairs);
        let gt = ids(&idx.scan(&Predicate::GreaterThan(Value::Int(pivot))));
        let lte = ids(&idx.scan(&Predicate::LessThanOrEqual(Value::Int(pivot))));

        // Every inserted doc is in exactly one side.
        let all: BTreeSet<DocId> = oracle.iter().map(|(d, _)| *d).collect();
        let mut union = gt.clone();
        union.extend(&lte);
        prop_assert_eq!(union, all);

        // Disjoint.
        let intersect: BTreeSet<DocId> = gt.intersection(&lte).copied().collect();
        prop_assert!(intersect.is_empty(), "overlap: {:?}", intersect);
    }

    /// `InSet([v1, v2])` equals the union of `Equals(v_i)`.
    #[test]
    fn in_set_equals_union_of_equals(
        pairs in arb_pairs(),
        members in proptest::collection::vec(-50i64..=50, 1..=4),
    ) {
        let (idx, _) = build_index(&pairs);
        let in_set: BTreeSet<Value> = members.iter().map(|v| Value::Int(*v)).collect();
        let observed = ids(&idx.scan(&Predicate::InSet(in_set)));
        let mut expected = BTreeSet::new();
        for v in &members {
            let part = ids(&idx.scan(&Predicate::Equals(Value::Int(*v))));
            expected.extend(part);
        }
        prop_assert_eq!(observed, expected);
    }

    /// Insert + remove on the same `(doc_id, value)` leaves the doc
    /// absent from subsequent scans.
    #[test]
    fn insert_then_remove_round_trip(
        pairs in arb_pairs(),
        target_doc in 1u64..=20,
    ) {
        let mut idx = BTreeIndex::new("v");
        for (doc, val) in &pairs {
            idx.insert(*doc, Value::Int(*val));
        }
        // Find target_doc's value if it's in the corpus, then remove it.
        let target_val = pairs.iter().find(|(d, _)| *d == target_doc).map(|(_, v)| *v);
        if let Some(v) = target_val {
            idx.remove(target_doc, &Value::Int(v));
            // After removal, no scan over the value space includes target_doc.
            for probe in -50i64..=50 {
                let s = ids(&idx.scan(&Predicate::Equals(Value::Int(probe))));
                prop_assert!(!s.contains(&target_doc), "doc survived removal at probe={probe}");
            }
        }
    }
}

/// Sanity: `Equals` on an empty index returns an empty posting list.
#[test]
fn equals_on_empty_returns_empty() {
    let idx = BTreeIndex::new("v");
    let pl = idx.scan(&Predicate::Equals(Value::Int(0)));
    assert!(pl.doc_ids().next().is_none());
}

/// Sanity: insert + scan returns the doc once (no duplicates from
/// the bucket dedup).
#[test]
fn duplicate_inserts_dedupe() {
    let mut idx = BTreeIndex::new("v");
    idx.insert(1, Value::Int(42));
    idx.insert(1, Value::Int(42));
    idx.insert(1, Value::Int(42));
    let pl = idx.scan(&Predicate::Equals(Value::Int(42)));
    let docs: Vec<DocId> = pl.doc_ids().collect();
    assert_eq!(docs, vec![1]);
}
