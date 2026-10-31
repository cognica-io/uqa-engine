//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ColumnType, RowSchema};

#[test]
fn borrowed_column_keys_preserve_owned_hash_and_identity() {
    let hasher = ahash::RandomState::new();
    for (qualifier, column) in [
        (None, "a.b"),
        (Some("a"), "b"),
        (Some("a.b"), "c\"d"),
        (Some(""), ""),
        (None, "한글"),
    ] {
        let owned = qualifier.map_or_else(
            || ColumnIdentity::unqualified(column),
            |q| ColumnIdentity::qualified(q, column),
        );
        let borrowed = (qualifier, column);
        assert_eq!(
            hasher.hash_one(&owned),
            hasher.hash_one(&borrowed as &dyn ColumnKey)
        );
        assert!((&owned as &dyn ColumnKey) == (&borrowed as &dyn ColumnKey));
        let mut index = crate::schema::SchemaMap::default();
        index.insert(owned, 7usize);
        assert_eq!(index.get(&borrowed as &dyn ColumnKey), Some(&7));
    }
}

#[test]
fn schema_slot_observation_borrows_wide_keys_and_wildcard_metadata() {
    let qualifier = "q".repeat(64 * 1024);
    let column = "c".repeat(64 * 1024);
    let schema = RowSchema::with_qualified_types(
        &qualifier,
        vec![column.clone()],
        vec![Some(ColumnType::Integer)],
    );
    let alias = "a".repeat(64 * 1024);
    let schema = RowSchema::with_physical_identity_aliases(
        &schema,
        &[(
            ColumnIdentity::unqualified(&alias),
            0,
            Some(ColumnType::Integer),
        )],
    );
    let allocation = allocation_counter::measure(|| {
        for _ in 0..16 {
            assert_eq!(schema.qualified_slot(&qualifier, &column), Some(0));
            assert_eq!(schema.column_slot(&alias), Some(0));
            assert_eq!(schema.column_slot("absent"), None);
            let mut slots = schema.qualified_star_slots(&qualifier);
            assert_eq!(slots.next(), Some(0));
            assert_eq!(slots.next(), None);
        }
    });
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
}
