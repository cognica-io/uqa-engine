//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::schema_construction::count_builds;

#[test]
fn correlated_scope_builds_lookup_maps_once_with_or_without_virtual_columns() {
    for width in [1, 64, 1024] {
        let columns = (0..width).map(|i| format!("c{i}")).collect::<Vec<_>>();
        let input = RowSchema::with_qualified_types(
            "inner",
            columns.clone(),
            vec![Some(ColumnType::Text); width],
        )
        .with_column_collations(std::iter::repeat_n(Some(950), width));
        for virtual_column in [false, true] {
            let outer = RowSchema::with_qualified_types(
                "outer",
                columns.clone(),
                vec![Some(ColumnType::Text); width],
            )
            .with_column_collations(std::iter::repeat_n(Some(951), width));
            let identity = ColumnIdentity::qualified("generated", "value");
            let outer = if virtual_column {
                RowSchema::with_typed_virtual_identities(
                    &outer,
                    &[(identity.clone(), Some(ColumnType::Text))],
                )
                .with_virtual_collation(&identity, 951)
                .unwrap()
            } else {
                outer
            };
            let (scope, builds) = count_builds(|| RowSchema::with_outer_schema(&input, &outer));
            assert_eq!(builds, 1, "width={width}, virtual={virtual_column}");
            assert_eq!(scope.columns(), input.columns());
            assert_eq!(scope.column_types(), input.column_types());
            assert_eq!(scope.physical_width(), width * 2);
            for (i, column) in columns.iter().enumerate() {
                assert_eq!(scope.column_slot(column), Some(i));
                assert_eq!(scope.qualified_slot("outer", column), Some(width + i));
                assert!(scope.resolves_local_column(None, column));
                assert!(!scope.resolves_local_column(Some("outer"), column));
                assert_eq!(scope.collation_of(column), Some(950));
                assert_eq!(scope.qualified_collation("outer", column), Some(951));
            }
            assert_eq!(
                scope.has_qualified_column("generated", "value"),
                virtual_column
            );
            if virtual_column {
                assert_eq!(
                    scope.qualified_type("generated", "value"),
                    Some(&ColumnType::Text)
                );
                assert_eq!(scope.qualified_collation("generated", "value"), Some(951));
                assert_eq!(scope.qualified_slot("generated", "value"), None);
            }
        }
    }
}

#[test]
fn unchanged_virtual_bindings_share_indexes_and_new_bindings_preserve_the_source() {
    let identity = ColumnIdentity::qualified("local", "value");
    let input = RowSchema::with_qualified_types(
        "local",
        vec!["value".into()],
        vec![Some(ColumnType::Text)],
    )
    .with_column_collations([Some(951)]);
    for additions in [vec![], vec![(identity, Some(ColumnType::Integer))]] {
        let (same, builds) =
            count_builds(|| RowSchema::with_typed_virtual_identities(&input, &additions));
        assert!(same.shares_index(&input));
        assert_eq!(builds, 0);
        assert_eq!(same, input);
    }
    let new = RowSchema::with_typed_virtual_identities(
        &input,
        &[(
            ColumnIdentity::qualified("generated", "value"),
            Some(ColumnType::Integer),
        )],
    );
    assert!(!input.has_qualified_column("generated", "value"));
    assert_eq!(
        new.qualified_type("generated", "value"),
        Some(&ColumnType::Integer)
    );
    assert_eq!(new.qualified_collation("local", "value"), Some(951));
    assert_eq!(new.physical_width(), input.physical_width());
}
