//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::constraint_metadata::{CatalogObjectAllocator, CatalogOidClass};

struct Allocator(u8);

impl CatalogObjectAllocator for Allocator {
    fn allocate_object_id(&mut self, _kind: &str) -> ConstraintMetadataResult<[u8; 16]> {
        self.0 += 1;
        Ok([self.0; 16])
    }

    fn allocate_catalog_oid(
        &mut self,
        _class: CatalogOidClass,
        object_id: &[u8; 16],
    ) -> ConstraintMetadataResult<i64> {
        Ok(16_384 + i64::from(object_id[0]))
    }
}

const fn partition(id: u8, parent: Option<u8>) -> ReferencedPartition {
    ReferencedPartition {
        partition: [id; 16],
        parent: match parent {
            Some(parent) => Some([parent; 16]),
            None => None,
        },
    }
}

fn names(constraints: &[ReferencedPartitionConstraint]) -> Vec<(&str, u8)> {
    constraints
        .iter()
        .map(|constraint| (constraint.name.as_str(), constraint.partition[0]))
        .collect()
}

const fn valid(name: &str) -> ReferencingConstraint<'_> {
    ReferencingConstraint {
        name,
        validated: true,
        enforced: true,
    }
}

#[test]
fn creation_names_every_partition_from_the_foreign_key_and_later_partitions_from_their_parent() {
    let mut constraints = Vec::new();
    let mut used = BTreeSet::from(["fk_a_fkey".to_string(), "fk_a_fkey_2".to_string()]);
    let mut allocate = Allocator(0);
    // pk1, then pk2 with its partition pk21; a name the schema holds is skipped.
    let tree = [
        partition(1, None),
        partition(2, None),
        partition(21, Some(2)),
    ];
    assert!(reconcile_referenced_partition_constraints(
        &mut constraints,
        valid("fk_a_fkey"),
        &tree,
        &mut used,
        &mut allocate,
    )
    .unwrap());
    assert_eq!(
        names(&constraints),
        [("fk_a_fkey_1", 1), ("fk_a_fkey_3", 2), ("fk_a_fkey_4", 21)]
    );
    assert!(constraints
        .iter()
        .all(|constraint| constraint.catalog_identity.is_valid() && constraint.validated));
    assert!(!reconcile_referenced_partition_constraints(
        &mut constraints,
        valid("fk_a_fkey"),
        &tree,
        &mut used,
        &mut allocate,
    )
    .unwrap());
    // A partition that joins pk2 later takes its name from pk2's constraint, and a partitioned partition that joins pk names its whole subtree from the foreign key.
    let tree = [
        partition(1, None),
        partition(2, None),
        partition(21, Some(2)),
        partition(22, Some(2)),
        partition(3, None),
        partition(31, Some(3)),
    ];
    reconcile_referenced_partition_constraints(
        &mut constraints,
        valid("fk_a_fkey"),
        &tree,
        &mut used,
        &mut allocate,
    )
    .unwrap();
    assert_eq!(
        names(&constraints)[3..],
        [
            ("fk_a_fkey_3_1", 22),
            ("fk_a_fkey_5", 3),
            ("fk_a_fkey_6", 31)
        ]
    );
    // Detaching pk2 removes the constraints of its subtree.
    assert!(reconcile_referenced_partition_constraints(
        &mut constraints,
        valid("fk_a_fkey"),
        &[
            partition(1, None),
            partition(3, None),
            partition(31, Some(3))
        ],
        &mut used,
        &mut allocate,
    )
    .unwrap());
    assert_eq!(
        names(&constraints),
        [("fk_a_fkey_1", 1), ("fk_a_fkey_5", 3), ("fk_a_fkey_6", 31)]
    );
}

#[test]
fn derived_constraints_follow_the_validity_of_the_constraint_they_join() {
    let mut constraints = Vec::new();
    let mut used = BTreeSet::new();
    let mut allocate = Allocator(0);
    let unvalidated = ReferencingConstraint {
        name: "fk_a_fkey",
        validated: false,
        enforced: true,
    };
    let tree = [partition(1, None), partition(2, None)];
    reconcile_referenced_partition_constraints(
        &mut constraints,
        unvalidated,
        &tree,
        &mut used,
        &mut allocate,
    )
    .unwrap();
    assert!(constraints.iter().all(|constraint| !constraint.validated));
    // Validating one derived constraint keeps the others unvalidated, and a partition joining it inherits its validity.
    constraints[1].validated = true;
    reconcile_referenced_partition_constraints(
        &mut constraints,
        unvalidated,
        &[
            partition(1, None),
            partition(2, None),
            partition(21, Some(2)),
        ],
        &mut used,
        &mut allocate,
    )
    .unwrap();
    assert_eq!(
        constraints
            .iter()
            .map(|constraint| constraint.validated)
            .collect::<Vec<_>>(),
        [false, true, true]
    );
    // A foreign key that is no longer enforced validates none of them.
    reconcile_referenced_partition_constraints(
        &mut constraints,
        ReferencingConstraint {
            name: "fk_a_fkey",
            validated: false,
            enforced: false,
        },
        &[
            partition(1, None),
            partition(2, None),
            partition(21, Some(2)),
        ],
        &mut used,
        &mut allocate,
    )
    .unwrap();
    assert!(constraints.iter().all(|constraint| !constraint.validated));
}
