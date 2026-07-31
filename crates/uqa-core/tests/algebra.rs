//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property tests for the Boolean algebra over [`DocSet`].
//!
//! Payload-bearing [`PostingList`] values project onto this carrier, but are
//! not themselves a Boolean algebra: their collision policy adds scores and
//! uses right-hand field precedence.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use uqa_core::{DocId, DocSet, Payload, PostingEntry, PostingList, Value};

fn id_set() -> impl Strategy<Value = BTreeSet<DocId>> {
    prop::collection::btree_set(0u64..32, 0..16)
}

fn id_set_pair() -> impl Strategy<Value = (BTreeSet<DocId>, BTreeSet<DocId>)> {
    (id_set(), id_set())
}

fn id_set_triple() -> impl Strategy<Value = (BTreeSet<DocId>, BTreeSet<DocId>, BTreeSet<DocId>)> {
    (id_set(), id_set(), id_set())
}

fn docs(ids: &BTreeSet<DocId>) -> DocSet {
    ids.iter().copied().collect()
}

fn universe(sets: &[&BTreeSet<DocId>]) -> DocSet {
    sets.iter().flat_map(|set| set.iter().copied()).collect()
}

proptest! {
    #[test]
    fn idempotence(ids in id_set()) {
        let a = docs(&ids);
        prop_assert_eq!(&a | &a, a.clone());
        prop_assert_eq!(&a & &a, a);
    }

    #[test]
    fn commutativity((left, right) in id_set_pair()) {
        let a = docs(&left);
        let b = docs(&right);
        prop_assert_eq!(&a | &b, &b | &a);
        prop_assert_eq!(&a & &b, &b & &a);
    }

    #[test]
    fn associativity((first, second, third) in id_set_triple()) {
        let a = docs(&first);
        let b = docs(&second);
        let c = docs(&third);
        prop_assert_eq!(&(&a | &b) | &c, &a | &(&b | &c));
        prop_assert_eq!(&(&a & &b) & &c, &a & &(&b & &c));
    }

    #[test]
    fn distributivity_or_over_and((first, second, third) in id_set_triple()) {
        let a = docs(&first);
        let b = docs(&second);
        let c = docs(&third);
        prop_assert_eq!(&a | &(&b & &c), &(&a | &b) & &(&a | &c));
    }

    #[test]
    fn distributivity_and_over_or((first, second, third) in id_set_triple()) {
        let a = docs(&first);
        let b = docs(&second);
        let c = docs(&third);
        prop_assert_eq!(&a & &(&b | &c), &(&a & &b) | &(&a & &c));
    }

    #[test]
    fn identity_with_empty_for_union(ids in id_set()) {
        let a = docs(&ids);
        let empty = DocSet::new();
        prop_assert_eq!(&a | &empty, a.clone());
        prop_assert_eq!(&empty | &a, a);
    }

    #[test]
    fn identity_with_universe_for_intersect((left, right) in id_set_pair()) {
        let a = docs(&left);
        let all = universe(&[&left, &right]);
        prop_assert_eq!(&a & &all, a);
    }

    #[test]
    fn annihilator_empty_for_intersect(ids in id_set()) {
        let a = docs(&ids);
        let empty = DocSet::new();
        prop_assert_eq!(&a & &empty, empty);
    }

    #[test]
    fn annihilator_universe_for_union((left, right) in id_set_pair()) {
        let a = docs(&left);
        let all = universe(&[&left, &right]);
        prop_assert_eq!(&a | &all, all);
    }

    #[test]
    fn complement_law_union((left, right) in id_set_pair()) {
        let a = docs(&left);
        let all = universe(&[&left, &right]);
        prop_assert_eq!(&a | &a.complement(&all), all);
    }

    #[test]
    fn complement_law_intersect((left, right) in id_set_pair()) {
        let a = docs(&left);
        let all = universe(&[&left, &right]);
        prop_assert_eq!(&a & &a.complement(&all), DocSet::new());
    }

    #[test]
    fn de_morgan_union((first, second, third) in id_set_triple()) {
        let a = docs(&first);
        let b = docs(&second);
        let all = universe(&[&first, &second, &third]);
        let lhs = (&a | &b).complement(&all);
        let rhs = &a.complement(&all) & &b.complement(&all);
        prop_assert_eq!(lhs, rhs);
    }

    #[test]
    fn de_morgan_intersect((first, second, third) in id_set_triple()) {
        let a = docs(&first);
        let b = docs(&second);
        let all = universe(&[&first, &second, &third]);
        let lhs = (&a & &b).complement(&all);
        let rhs = &a.complement(&all) | &b.complement(&all);
        prop_assert_eq!(lhs, rhs);
    }

    #[test]
    fn difference_equals_intersect_complement((left, right) in id_set_pair()) {
        let a = docs(&left);
        let b = docs(&right);
        let all = universe(&[&left, &right]);
        prop_assert_eq!(&a - &b, &a & &b.complement(&all));
    }

    #[test]
    fn sort_invariant_holds_after_every_operation((left, right) in id_set_pair()) {
        let a = docs(&left);
        let b = docs(&right);
        for result in [&a | &b, &a & &b, &a - &b] {
            prop_assert!(result.as_slice().windows(2).all(|window| window[0] < window[1]));
        }
    }

    #[test]
    fn standard_set_round_trip(ids in id_set()) {
        let actual: BTreeSet<_> = docs(&ids).into_iter().collect();
        prop_assert_eq!(actual, ids);
    }

    #[test]
    fn doc_set_to_posting_support_round_trip(ids in id_set()) {
        let support = docs(&ids);
        prop_assert_eq!(PostingList::from_support(&support).support(), support);
    }
}

#[test]
fn posting_support_reconstruction_does_not_restore_payload() {
    let decorated = PostingList::from_unsorted(vec![PostingEntry::new(
        7,
        Payload {
            positions: vec![1, 4],
            score: 2.5,
            fields: BTreeMap::from([("field".to_string(), Value::Str("body".to_string()))]),
        },
    )]);

    let reconstructed = PostingList::from_support(&decorated.support());
    assert_eq!(reconstructed.support(), decorated.support());
    assert_ne!(reconstructed, decorated);
}

#[test]
fn posting_payload_merge_is_neither_idempotent_nor_commutative() {
    let left = PostingList::from_unsorted(vec![PostingEntry::new(
        1,
        Payload {
            score: 1.0,
            fields: BTreeMap::from([("source".to_string(), Value::Str("left".to_string()))]),
            ..Payload::default()
        },
    )]);
    let right = PostingList::from_unsorted(vec![PostingEntry::new(
        1,
        Payload {
            score: 2.0,
            fields: BTreeMap::from([("source".to_string(), Value::Str("right".to_string()))]),
            ..Payload::default()
        },
    )]);

    assert_ne!(left.merge_union(&left), left);
    assert_ne!(left.merge_union(&right), right.merge_union(&left));
    assert_eq!(
        left.merge_union(&right).support(),
        right.merge_union(&left).support()
    );
}
