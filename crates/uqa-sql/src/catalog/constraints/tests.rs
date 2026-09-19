//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_foreign_key_selects_its_renamed_row_before_partition_family_members() {
    let original = ConstraintIdentity {
        relation: RelationIdentity::new("public", "parent"),
        name: "fk".into(),
        object_id: Some([1; 16]),
    };
    let child = ConstraintIdentity {
        relation: RelationIdentity::new("public", "child"),
        ..original.clone()
    };
    let renamed = ConstraintIdentity {
        name: "renamed".into(),
        ..original.clone()
    };
    let live = vec![child, renamed.clone()];
    let relations = live
        .iter()
        .map(|identity| identity.relation.clone())
        .collect();
    assert_eq!(
        find_live_constraint_identity(&live, &relations, &original),
        Some(&renamed)
    );
    assert!(find_live_constraint_identity(&live[..1], &relations, &original).is_none());
    let replacement = ConstraintIdentity {
        object_id: Some([2; 16]),
        ..original.clone()
    };
    assert!(find_live_constraint_identity(&[replacement], &relations, &original).is_none());
}

#[test]
fn legacy_constraints_use_exact_names_and_relation_rename_retains_a_durable_identity() {
    let original = ConstraintIdentity {
        relation: RelationIdentity::new("public", "old"),
        name: "fk".into(),
        object_id: Some([1; 16]),
    };
    let moved = ConstraintIdentity {
        relation: RelationIdentity::new("moved", "new"),
        ..original.clone()
    };
    let live = vec![moved.clone()];
    let relations = live
        .iter()
        .map(|identity| identity.relation.clone())
        .collect();
    assert_eq!(
        find_live_constraint_identity(&live, &relations, &original),
        Some(&moved)
    );
    let legacy = ConstraintIdentity {
        object_id: None,
        ..original
    };
    assert!(find_live_constraint_identity(&live, &relations, &legacy).is_none());
}
