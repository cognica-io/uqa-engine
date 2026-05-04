//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property tests for the Boolean algebra over [`PostingList`].
//!
//! Identities (Theorem 2.1.2, Paper 1) hold over the doc-id set, not over
//! payload scores: `union` and `intersect` add scores on collision, so
//! `a | a` differs from `a` at the score level. Tests therefore compare
//! via [`PostingList::doc_id_set`].
//!
//! The universal set `U` is the union of all generated id sets in each
//! test, which guarantees every input is a subset of `U` — a precondition
//! for the identity, annihilator, and complement laws.

use std::collections::BTreeSet;

use proptest::prelude::*;
use uqa_core::{DocId, Payload, PostingEntry, PostingList};

fn pl_from_ids(ids: &BTreeSet<DocId>) -> PostingList {
    PostingList::from_unsorted(
        ids.iter()
            .map(|&id| PostingEntry::new(id, Payload::default()))
            .collect(),
    )
}

fn id_set() -> impl Strategy<Value = BTreeSet<DocId>> {
    prop::collection::btree_set(0u64..32, 0..16)
}

fn id_set_pair() -> impl Strategy<Value = (BTreeSet<DocId>, BTreeSet<DocId>)> {
    (id_set(), id_set())
}

fn id_set_triple() -> impl Strategy<Value = (BTreeSet<DocId>, BTreeSet<DocId>, BTreeSet<DocId>)> {
    (id_set(), id_set(), id_set())
}

fn ids_of(pl: &PostingList) -> BTreeSet<DocId> {
    pl.doc_id_set()
}

fn entries_strictly_ascending(pl: &PostingList) -> bool {
    pl.entries().windows(2).all(|w| w[0].doc_id < w[1].doc_id)
}

proptest! {
    #[test]
    fn idempotence(s in id_set()) {
        let a = pl_from_ids(&s);
        prop_assert!((&a | &a).doc_ids_eq(&a));
        prop_assert!((&a & &a).doc_ids_eq(&a));
    }

    #[test]
    fn commutativity((sa, sb) in id_set_pair()) {
        let a = pl_from_ids(&sa);
        let b = pl_from_ids(&sb);
        prop_assert!((&a | &b).doc_ids_eq(&(&b | &a)));
        prop_assert!((&a & &b).doc_ids_eq(&(&b & &a)));
    }

    #[test]
    fn associativity((sa, sb, sc) in id_set_triple()) {
        let a = pl_from_ids(&sa);
        let b = pl_from_ids(&sb);
        let c = pl_from_ids(&sc);
        prop_assert!(((&(&a | &b)) | &c).doc_ids_eq(&(&a | &(&b | &c))));
        prop_assert!(((&(&a & &b)) & &c).doc_ids_eq(&(&a & &(&b & &c))));
    }

    #[test]
    fn distributivity_or_over_and((sa, sb, sc) in id_set_triple()) {
        let a = pl_from_ids(&sa);
        let b = pl_from_ids(&sb);
        let c = pl_from_ids(&sc);
        let lhs = &a | &(&b & &c);
        let rhs = &(&a | &b) & &(&a | &c);
        prop_assert!(lhs.doc_ids_eq(&rhs));
    }

    #[test]
    fn distributivity_and_over_or((sa, sb, sc) in id_set_triple()) {
        let a = pl_from_ids(&sa);
        let b = pl_from_ids(&sb);
        let c = pl_from_ids(&sc);
        let lhs = &a & &(&b | &c);
        let rhs = &(&a & &b) | &(&a & &c);
        prop_assert!(lhs.doc_ids_eq(&rhs));
    }

    #[test]
    fn identity_with_empty_for_union(s in id_set()) {
        let a = pl_from_ids(&s);
        let empty = PostingList::new();
        prop_assert!((&a | &empty).doc_ids_eq(&a));
        prop_assert!((&empty | &a).doc_ids_eq(&a));
    }

    #[test]
    fn identity_with_universal_for_intersect((sa, sb) in id_set_pair()) {
        let a = pl_from_ids(&sa);
        let universal_ids: BTreeSet<DocId> = sa.union(&sb).copied().collect();
        let universal = pl_from_ids(&universal_ids);
        prop_assert!((&a & &universal).doc_ids_eq(&a));
    }

    #[test]
    fn annihilator_empty_for_intersect(s in id_set()) {
        let a = pl_from_ids(&s);
        let empty = PostingList::new();
        prop_assert!((&a & &empty).doc_ids_eq(&empty));
    }

    #[test]
    fn annihilator_universal_for_union((sa, sb) in id_set_pair()) {
        let a = pl_from_ids(&sa);
        let universal_ids: BTreeSet<DocId> = sa.union(&sb).copied().collect();
        let universal = pl_from_ids(&universal_ids);
        prop_assert!((&a | &universal).doc_ids_eq(&universal));
    }

    #[test]
    fn complement_law_union((sa, sb) in id_set_pair()) {
        let a = pl_from_ids(&sa);
        let universal_ids: BTreeSet<DocId> = sa.union(&sb).copied().collect();
        let universal = pl_from_ids(&universal_ids);
        let complement_a = a.complement(&universal);
        prop_assert!((&a | &complement_a).doc_ids_eq(&universal));
    }

    #[test]
    fn complement_law_intersect((sa, sb) in id_set_pair()) {
        let a = pl_from_ids(&sa);
        let universal_ids: BTreeSet<DocId> = sa.union(&sb).copied().collect();
        let universal = pl_from_ids(&universal_ids);
        let complement_a = a.complement(&universal);
        let empty = PostingList::new();
        prop_assert!((&a & &complement_a).doc_ids_eq(&empty));
    }

    #[test]
    fn de_morgan_union((sa, sb, sc) in id_set_triple()) {
        let a = pl_from_ids(&sa);
        let b = pl_from_ids(&sb);
        let universal_ids: BTreeSet<DocId> =
            sa.union(&sb).copied().collect::<BTreeSet<_>>()
                .union(&sc).copied().collect();
        let universal = pl_from_ids(&universal_ids);
        let lhs = (&a | &b).complement(&universal);
        let rhs = &a.complement(&universal) & &b.complement(&universal);
        prop_assert!(lhs.doc_ids_eq(&rhs));
    }

    #[test]
    fn de_morgan_intersect((sa, sb, sc) in id_set_triple()) {
        let a = pl_from_ids(&sa);
        let b = pl_from_ids(&sb);
        let universal_ids: BTreeSet<DocId> =
            sa.union(&sb).copied().collect::<BTreeSet<_>>()
                .union(&sc).copied().collect();
        let universal = pl_from_ids(&universal_ids);
        let lhs = (&a & &b).complement(&universal);
        let rhs = &a.complement(&universal) | &b.complement(&universal);
        prop_assert!(lhs.doc_ids_eq(&rhs));
    }

    #[test]
    fn difference_equals_intersect_complement((sa, sb) in id_set_pair()) {
        let a = pl_from_ids(&sa);
        let b = pl_from_ids(&sb);
        let universal_ids: BTreeSet<DocId> = sa.union(&sb).copied().collect();
        let universal = pl_from_ids(&universal_ids);
        let lhs = &a - &b;
        let rhs = &a & &b.complement(&universal);
        prop_assert!(lhs.doc_ids_eq(&rhs));
    }

    #[test]
    fn sort_invariant_holds_after_every_op((sa, sb) in id_set_pair()) {
        let a = pl_from_ids(&sa);
        let b = pl_from_ids(&sb);
        prop_assert!(entries_strictly_ascending(&(&a | &b)));
        prop_assert!(entries_strictly_ascending(&(&a & &b)));
        prop_assert!(entries_strictly_ascending(&(&a - &b)));
    }

    #[test]
    fn doc_set_round_trip(s in id_set()) {
        let a = pl_from_ids(&s);
        prop_assert_eq!(ids_of(&a), s);
    }
}
