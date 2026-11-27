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

#[test]
fn name_type_and_collation_probes_borrow_each_visible_and_hidden_identity() {
    use crate::{schema::ProjectedSlot, ScalarExpr};
    let qualifier = "q".repeat(4096);
    let name = "n".repeat(4096);
    let schema = RowSchema::with_qualified_types(
        &qualifier,
        vec![name.clone()],
        vec![Some(ColumnType::Text)],
    )
    .with_column_collations([Some(950)]);
    let schema = RowSchema::with_identity_aliases(
        &schema,
        &[
            (ColumnIdentity::unqualified("alias"), 0),
            (ColumnIdentity::qualified("other", "alias"), 0),
        ],
    );
    let schema = RowSchema::with_typed_virtual_identities(
        &schema,
        &[
            (
                ColumnIdentity::unqualified("virtual"),
                Some(ColumnType::Text),
            ),
            (
                ColumnIdentity::qualified("v", "virtual"),
                Some(ColumnType::Text),
            ),
        ],
    );
    let alias = ScalarExpr::QualifiedColumn {
        qualifier: "other".into(),
        column: "alias".into(),
    };
    let projected = Some(ProjectedSlot::BoundInput {
        physical: Some(0),
        collation: Some(950),
    });
    let allocation = allocation_counter::measure(|| {
        for _ in 0..16 {
            assert!(schema.has_qualified_column(&qualifier, &name));
            assert!(schema.has_unqualified_column("virtual"));
            assert!(!schema.has_qualified_column("missing", &name));
            assert!(!schema.qualified_column_is_ambiguous(&qualifier, &name));
            assert_eq!(schema.qualified_position(&qualifier, &name), Some(0));
            assert_eq!(
                schema.qualified_type("v", "virtual"),
                Some(&ColumnType::Text)
            );
            assert_eq!(schema.type_of("alias"), Some(&ColumnType::Text));
            assert_eq!(schema.type_of("virtual"), Some(&ColumnType::Text));
            assert_eq!(schema.type_of("missing"), None);
            assert_eq!(schema.qualified_collation(&qualifier, &name), Some(950));
            assert_eq!(schema.collation_of("alias"), Some(950));
            assert_eq!(schema.collation_of("virtual"), Some(100));
            assert_eq!(schema.projection_input(&alias), projected);
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn metadata_overlays_preserve_name_scope_without_rebuilding_lookup_indexes() {
    use crate::ast::InternalRelationId;
    let local =
        RowSchema::with_qualified_types("local", vec!["x".into()], vec![Some(ColumnType::Integer)]);
    let outer =
        RowSchema::with_qualified_types("outer", vec!["y".into()], vec![Some(ColumnType::Integer)]);
    let schema = RowSchema::with_outer_schema(&local, &outer);
    let internal = InternalRelationId::allocate().column(0);
    assert!(!schema.resolves_local_column(Some("outer"), "y"));
    let (derived, builds) = crate::schema::schema_construction::count_builds(|| {
        let schema = RowSchema::with_physical_internal_aliases(
            &schema,
            &[(internal, 0, Some(ColumnType::Integer))],
        );
        let schema = RowSchema::with_score_source(&schema, Some("local"), internal);
        let schema = RowSchema::with_rebound_score_sources(&schema, Some("scored"));
        let schema = RowSchema::with_typed_conflicting_virtual_identities(
            &schema,
            &[(
                ColumnIdentity::qualified("local", "x"),
                Some(ColumnType::Integer),
            )],
        );
        let schema = RowSchema::with_open_columns(&schema, Some("document"));
        RowSchema::with_wildcard_hidden_positions(&schema, [0])
    });
    assert_eq!(builds, 0);
    assert!(!derived.resolves_local_column(Some("outer"), "y"));
    assert!(derived.qualified_column_is_ambiguous("local", "x"));
    assert_eq!(derived.score_source_slot(Some("scored")), Some(0));
    assert!(derived.columns_are_open(Some("document")));
    assert_eq!(schema.score_source_slot(Some("scored")), None);
    assert!(!schema.qualified_column_is_ambiguous("local", "x"));
}
