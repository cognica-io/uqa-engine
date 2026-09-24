//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn identity(id: u8, oid: i64) -> ConstraintCatalogIdentity {
    ConstraintCatalogIdentity {
        object_id: [id; 16],
        oid,
    }
}

#[test]
fn legacy_address_collisions_preserve_current_and_noncolliding_rows() {
    let rows = [
        (identity(1, 21_000), "key", true),
        (identity(2, 21_000), "NOT NULL", false),
        (identity(3, 21_001), "CHECK", true),
        (identity(4, 21_001), "CHECK", true),
        (identity(5, 21_002), "key", true),
    ];
    let replacements = coordinate_addresses(&rows).unwrap();
    assert_eq!(replacements.len(), 2);
    assert!(replacements.contains_key(&[1; 16]));
    assert!(replacements.contains_key(&[4; 16]));
    let final_addresses: BTreeSet<_> = rows
        .iter()
        .map(|(identity, _, _)| {
            *replacements
                .get(&identity.object_id)
                .unwrap_or(&identity.oid)
        })
        .collect();
    assert_eq!(final_addresses.len(), rows.len());
}

#[test]
fn current_duplicate_addresses_and_duplicate_incarnations_are_never_repaired() {
    for rows in [
        [
            (identity(1, 21_000), "key", false),
            (identity(2, 21_000), "CHECK", false),
        ],
        [
            (identity(1, 21_000), "key", true),
            (identity(1, 21_001), "CHECK", true),
        ],
        [
            (identity(0, 21_000), "key", true),
            (identity(1, 21_001), "CHECK", true),
        ],
    ] {
        assert!(coordinate_addresses(&rows).is_err());
    }
}
