//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Posting-list algebra and payload coverage.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use uqa_core::{
    DocId, DocSet, GeneralizedPayload, GeneralizedPostingEntry, GeneralizedPostingList, Payload,
    PostingEntry, PostingList, Value,
};

fn payload(score: f64) -> Payload {
    Payload {
        score,
        ..Payload::default()
    }
}

fn entry(doc_id: DocId, score: f64) -> PostingEntry {
    PostingEntry::new(doc_id, payload(score))
}

fn pl(ids: &[DocId]) -> PostingList {
    PostingList::from_unsorted(ids.iter().map(|id| entry(*id, 0.0)).collect())
}

fn pl_with(entries: &[(DocId, f64)]) -> PostingList {
    PostingList::from_unsorted(entries.iter().map(|(d, s)| entry(*d, *s)).collect())
}

fn sorted_posting_entries(list: &PostingList) -> bool {
    list.entries().windows(2).all(|w| w[0].doc_id < w[1].doc_id)
}

fn gentry(doc_ids: &[DocId]) -> GeneralizedPostingEntry {
    GeneralizedPostingEntry {
        doc_ids: doc_ids.to_vec(),
        payload: GeneralizedPayload::default(),
    }
}

fn gpl(tuples: &[&[DocId]]) -> GeneralizedPostingList {
    GeneralizedPostingList::from_unsorted(tuples.iter().map(|ids| gentry(ids)).collect())
}

fn sorted_generalized_entries(list: &GeneralizedPostingList) -> bool {
    list.entries()
        .windows(2)
        .all(|w| w[0].doc_ids <= w[1].doc_ids)
}

fn id_set() -> impl Strategy<Value = BTreeSet<DocId>> {
    prop::collection::btree_set(0u64..=50, 0..16)
}

fn posting_from_set(set: &BTreeSet<DocId>) -> PostingList {
    PostingList::from_unsorted(set.iter().map(|id| entry(*id, 0.0)).collect())
}

fn doc_set(set: &BTreeSet<DocId>) -> DocSet {
    set.iter().copied().collect()
}

proptest! {
    #[test]
    fn test_union_commutativity(a in id_set(), b in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        prop_assert_eq!(a.union(&b), b.union(&a));
    }

    #[test]
    fn test_intersect_commutativity(a in id_set(), b in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        prop_assert_eq!(a.intersect(&b), b.intersect(&a));
    }

    #[test]
    fn test_union_associativity(a in id_set(), b in id_set(), c in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        let c = doc_set(&c);
        let lhs = a.union(&b.union(&c));
        let rhs = a.union(&b).union(&c);
        prop_assert_eq!(lhs, rhs);
    }

    #[test]
    fn test_intersect_associativity(a in id_set(), b in id_set(), c in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        let c = doc_set(&c);
        let lhs = a.intersect(&b.intersect(&c));
        let rhs = a.intersect(&b).intersect(&c);
        prop_assert_eq!(lhs, rhs);
    }

    #[test]
    fn test_intersect_distributes_over_union(a in id_set(), b in id_set(), c in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        let c = doc_set(&c);
        let lhs = a.intersect(&b.union(&c));
        let rhs = a.intersect(&b).union(&a.intersect(&c));
        prop_assert_eq!(lhs, rhs);
    }

    #[test]
    fn test_union_distributes_over_intersect(a in id_set(), b in id_set(), c in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        let c = doc_set(&c);
        let lhs = a.union(&b.intersect(&c));
        let rhs = a.union(&b).intersect(&a.union(&c));
        prop_assert_eq!(lhs, rhs);
    }

    #[test]
    fn test_union_identity(a in id_set()) {
        let a = doc_set(&a);
        let empty = DocSet::new();
        prop_assert_eq!(a.union(&empty), a.clone());
        prop_assert_eq!(empty.union(&a), a);
    }

    #[test]
    fn test_intersect_identity(a in id_set()) {
        let a = doc_set(&a);
        let universal = DocSet::from((0u64..=50).collect::<Vec<_>>());
        prop_assert_eq!(a.intersect(&universal), a);
    }

    #[test]
    fn test_complement_union_is_universal(a in id_set()) {
        let a = doc_set(&a);
        let universal = DocSet::from((0u64..=50).collect::<Vec<_>>());
        let result = a.union(&a.complement(&universal));
        prop_assert_eq!(result, universal);
    }

    #[test]
    fn test_complement_intersect_is_empty(a in id_set()) {
        let a = doc_set(&a);
        let universal = DocSet::from((0u64..=50).collect::<Vec<_>>());
        prop_assert!(a.intersect(&a.complement(&universal)).is_empty());
    }

    #[test]
    fn test_sorted_invariant_after_union(a in id_set(), b in id_set()) {
        prop_assert!(sorted_posting_entries(&posting_from_set(&a).merge_union(&posting_from_set(&b))));
    }

    #[test]
    fn test_sorted_invariant_after_intersect(a in id_set(), b in id_set()) {
        prop_assert!(sorted_posting_entries(&posting_from_set(&a).merge_intersection(&posting_from_set(&b))));
    }

    #[test]
    fn test_sorted_invariant_after_complement(a in id_set()) {
        let universal = pl(&(0u64..=50).collect::<Vec<_>>());
        prop_assert!(sorted_posting_entries(&universal.exclude(&posting_from_set(&a))));
    }

    #[test]
    fn test_sorted_invariant_after_difference(a in id_set(), b in id_set()) {
        prop_assert!(sorted_posting_entries(&posting_from_set(&a).exclude(&posting_from_set(&b))));
    }

    #[test]
    fn test_difference_correctness(a in id_set(), b in id_set()) {
        let a_pl = posting_from_set(&a);
        let b_pl = posting_from_set(&b);
        let expected: DocSet = a.difference(&b).copied().collect();
        prop_assert_eq!(a_pl.exclude(&b_pl).support(), expected);
    }
}

#[test]
fn test_merge_payloads_positions() {
    let a = PostingList::from_unsorted(vec![PostingEntry::new(
        1,
        Payload {
            positions: vec![0, 2],
            score: 1.0,
            ..Payload::default()
        },
    )]);
    let b = PostingList::from_unsorted(vec![PostingEntry::new(
        1,
        Payload {
            positions: vec![1, 3],
            score: 0.5,
            ..Payload::default()
        },
    )]);
    assert_eq!(
        a.merge_union(&b).get_entry(1).unwrap().payload.positions,
        vec![0, 1, 2, 3]
    );
}

#[test]
fn test_merge_payloads_scores() {
    let result = pl_with(&[(1, 1.0)]).merge_union(&pl_with(&[(1, 0.5)]));
    assert!((result.get_entry(1).unwrap().payload.score - 1.5).abs() < 1e-12);
}

#[test]
fn test_merge_payloads_fields() {
    let mut fields_a = BTreeMap::new();
    fields_a.insert("a".to_string(), Value::Int(1));
    let mut fields_b = BTreeMap::new();
    fields_b.insert("b".to_string(), Value::Int(2));
    let a = PostingList::from_unsorted(vec![PostingEntry::new(
        1,
        Payload {
            fields: fields_a,
            ..Payload::default()
        },
    )]);
    let b = PostingList::from_unsorted(vec![PostingEntry::new(
        1,
        Payload {
            fields: fields_b,
            ..Payload::default()
        },
    )]);
    let union = a.merge_union(&b);
    let fields = &union.get_entry(1).unwrap().payload.fields;
    assert_eq!(fields.get("a"), Some(&Value::Int(1)));
    assert_eq!(fields.get("b"), Some(&Value::Int(2)));
}

#[test]
fn test_generalized_posting_list_union() {
    let result = gpl(&[&[1, 2], &[3, 4]]).merge_union(&gpl(&[&[1, 2], &[5, 6]]));
    assert_eq!(
        result.doc_ids_set(),
        BTreeSet::from([vec![1, 2], vec![3, 4], vec![5, 6]])
    );
}

#[test]
fn test_generalized_posting_list_sorted() {
    let list = gpl(&[&[5, 6], &[1, 2], &[3, 4]]);
    assert!(sorted_generalized_entries(&list));
}

#[test]
fn test_intersect_shared_tuples_only() {
    let result =
        gpl(&[&[1, 2], &[3, 4], &[5, 6]]).merge_intersection(&gpl(&[&[3, 4], &[5, 6], &[7, 8]]));
    assert_eq!(
        result.doc_ids_set(),
        BTreeSet::from([vec![3, 4], vec![5, 6]])
    );
}

#[test]
fn test_intersect_preserves_left_payload() {
    let mut left = gentry(&[1, 2]);
    left.payload
        .fields
        .insert("side".into(), Value::Str("left".into()));
    let result = GeneralizedPostingList::from_unsorted(vec![left]).merge_intersection(
        &GeneralizedPostingList::from_unsorted(vec![gentry(&[1, 2])]),
    );
    assert_eq!(
        result.entries()[0].payload.fields.get("side"),
        Some(&Value::Str("left".into()))
    );
}

#[test]
fn test_intersect_sorted_invariant() {
    let result =
        gpl(&[&[1, 2], &[3, 4], &[5, 6], &[7, 8]]).merge_intersection(&gpl(&[&[5, 6], &[1, 2]]));
    assert!(sorted_generalized_entries(&result));
}

#[test]
fn test_difference_self_minus_other() {
    let result = gpl(&[&[1, 2], &[3, 4], &[5, 6]]).exclude(&gpl(&[&[3, 4]]));
    assert_eq!(
        result.doc_ids_set(),
        BTreeSet::from([vec![1, 2], vec![5, 6]])
    );
}

#[test]
fn test_difference_preserves_payload() {
    let mut kept = gentry(&[1, 2]);
    kept.payload.fields.insert("score".into(), Value::Int(5));
    let mut dropped = gentry(&[3, 4]);
    dropped.payload.fields.insert("score".into(), Value::Int(7));
    let result =
        GeneralizedPostingList::from_unsorted(vec![kept, dropped]).exclude(&gpl(&[&[3, 4]]));
    assert_eq!(
        result.entries()[0].payload.fields.get("score"),
        Some(&Value::Int(5))
    );
}

#[test]
fn test_difference_sorted_invariant() {
    assert!(sorted_generalized_entries(
        &gpl(&[&[1, 2], &[3, 4], &[5, 6], &[7, 8]]).exclude(&gpl(&[&[3, 4]]))
    ));
}

#[test]
fn test_complement_universal_minus_self() {
    let universal = gpl(&[&[1, 2], &[3, 4], &[5, 6], &[7, 8]]);
    let result = universal.exclude(&gpl(&[&[3, 4], &[7, 8]]));
    assert_eq!(
        result.doc_ids_set(),
        BTreeSet::from([vec![1, 2], vec![5, 6]])
    );
}

#[test]
fn test_complement_of_empty_is_universal() {
    let universal = gpl(&[&[1, 2], &[3, 4]]);
    assert_eq!(universal.exclude(&GeneralizedPostingList::new()), universal);
}

#[test]
fn test_complement_of_universal_is_empty() {
    let universal = gpl(&[&[1, 2], &[3, 4]]);
    assert_eq!(universal.exclude(&universal).len(), 0);
}

#[test]
fn test_doc_ids_set_property() {
    assert_eq!(
        gpl(&[&[1, 2], &[3, 4], &[5, 6]]).doc_ids_set(),
        BTreeSet::from([vec![1, 2], vec![3, 4], vec![5, 6]])
    );
}

#[test]
fn test_doc_ids_set_empty() {
    assert!(GeneralizedPostingList::new().doc_ids_set().is_empty());
}

#[test]
fn test_explicit_tuple_intersection_merge() {
    let result = gpl(&[&[1, 2], &[3, 4], &[5, 6]]).merge_intersection(&gpl(&[&[3, 4], &[7, 8]]));
    assert_eq!(result.doc_ids_set(), BTreeSet::from([vec![3, 4]]));
}

#[test]
fn test_explicit_tuple_union_merge() {
    let result = gpl(&[&[1, 2], &[3, 4]]).merge_union(&gpl(&[&[3, 4], &[5, 6]]));
    assert_eq!(
        result.doc_ids_set(),
        BTreeSet::from([vec![1, 2], vec![3, 4], vec![5, 6]])
    );
}

#[test]
fn test_explicit_tuple_exclusion() {
    let result = gpl(&[&[1, 2], &[3, 4], &[5, 6]]).exclude(&gpl(&[&[3, 4]]));
    assert_eq!(
        result.doc_ids_set(),
        BTreeSet::from([vec![1, 2], vec![5, 6]])
    );
}

#[test]
fn test_eq_same_entries() {
    assert_eq!(gpl(&[&[1, 2], &[3, 4]]), gpl(&[&[1, 2], &[3, 4]]));
}

#[test]
fn test_eq_different_entries() {
    assert_ne!(gpl(&[&[1, 2], &[3, 4]]), gpl(&[&[1, 2], &[5, 6]]));
}

#[test]
fn test_eq_different_lengths() {
    assert_ne!(gpl(&[&[1, 2], &[3, 4]]), gpl(&[&[1, 2]]));
}

#[test]
fn test_eq_includes_payload_differences() {
    let mut a = gentry(&[1, 2]);
    a.payload.fields.insert("x".into(), Value::Int(1));
    let mut b = gentry(&[1, 2]);
    b.payload.fields.insert("x".into(), Value::Int(99));
    assert_ne!(
        GeneralizedPostingList::from_unsorted(vec![a]),
        GeneralizedPostingList::from_unsorted(vec![b])
    );
}

#[test]
fn test_intersect_with_empty() {
    assert!(gpl(&[&[1, 2], &[3, 4]])
        .merge_intersection(&GeneralizedPostingList::new())
        .is_empty());
    assert!(GeneralizedPostingList::new()
        .merge_intersection(&gpl(&[&[1, 2], &[3, 4]]))
        .is_empty());
}

#[test]
fn test_difference_with_empty() {
    let a = gpl(&[&[1, 2], &[3, 4]]);
    assert_eq!(a.exclude(&GeneralizedPostingList::new()), a);
    assert!(GeneralizedPostingList::new().exclude(&a).is_empty());
}

#[test]
fn test_union_with_empty() {
    let a = gpl(&[&[1, 2], &[3, 4]]);
    assert_eq!(a.merge_union(&GeneralizedPostingList::new()), a);
    assert_eq!(GeneralizedPostingList::new().merge_union(&a), a);
}

#[test]
fn test_intersect_no_overlap() {
    assert!(gpl(&[&[1, 2], &[3, 4]])
        .merge_intersection(&gpl(&[&[5, 6], &[7, 8]]))
        .is_empty());
}

#[test]
fn test_difference_no_overlap() {
    let a = gpl(&[&[1, 2], &[3, 4]]);
    assert_eq!(a.exclude(&gpl(&[&[5, 6], &[7, 8]])), a);
}

proptest! {
    #[test]
    fn test_de_morgan_intersect(a in id_set(), b in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        let universal = DocSet::from((0u64..=50).collect::<Vec<_>>());
        let lhs = a.intersect(&b).complement(&universal);
        let rhs = a.complement(&universal).union(&b.complement(&universal));
        prop_assert_eq!(lhs, rhs);
    }

    #[test]
    fn test_de_morgan_union(a in id_set(), b in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        let universal = DocSet::from((0u64..=50).collect::<Vec<_>>());
        let lhs = a.union(&b).complement(&universal);
        let rhs = a.complement(&universal).intersect(&b.complement(&universal));
        prop_assert_eq!(lhs, rhs);
    }

    #[test]
    fn test_distributivity(a in id_set(), b in id_set(), c in id_set()) {
        let a = doc_set(&a);
        let b = doc_set(&b);
        let c = doc_set(&c);
        let lhs = a.intersect(&b.union(&c));
        let rhs = a.intersect(&b).union(&a.intersect(&c));
        prop_assert_eq!(lhs, rhs);
    }
}

fn tuple_set_strategy() -> impl Strategy<Value = BTreeSet<Vec<DocId>>> {
    prop::collection::btree_set(prop::collection::vec(0u64..=10, 2), 0..8)
}

fn generalized_from_set(set: &BTreeSet<Vec<DocId>>) -> GeneralizedPostingList {
    GeneralizedPostingList::from_unsorted(
        set.iter()
            .map(|ids| GeneralizedPostingEntry {
                doc_ids: ids.clone(),
                payload: GeneralizedPayload::default(),
            })
            .collect(),
    )
}

proptest! {
    #[test]
    fn test_gpl_union_commutative(a in tuple_set_strategy(), b in tuple_set_strategy()) {
        let a = generalized_from_set(&a);
        let b = generalized_from_set(&b);
        prop_assert_eq!(a.merge_union(&b).doc_ids_set(), b.merge_union(&a).doc_ids_set());
    }

    #[test]
    fn test_gpl_intersect_commutative(a in tuple_set_strategy(), b in tuple_set_strategy()) {
        let a = generalized_from_set(&a);
        let b = generalized_from_set(&b);
        prop_assert_eq!(a.merge_intersection(&b).doc_ids_set(), b.merge_intersection(&a).doc_ids_set());
    }

    #[test]
    fn test_gpl_de_morgan_intersect(a in tuple_set_strategy(), b in tuple_set_strategy()) {
        let a = generalized_from_set(&a);
        let b = generalized_from_set(&b);
        let universal = a.merge_union(&b);
        let lhs = universal.exclude(&a.merge_intersection(&b)).doc_ids_set();
        let rhs = universal
            .exclude(&a)
            .merge_union(&universal.exclude(&b))
            .doc_ids_set();
        prop_assert_eq!(lhs, rhs);
    }
}
