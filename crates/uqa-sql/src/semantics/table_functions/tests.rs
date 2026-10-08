//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{builtin_table_function_overloads, upgrade_legacy_table_function_binding};
use crate::ast::{ColumnType, FunctionBinding};

fn legacy_binding(kind: &str) -> FunctionBinding {
    FunctionBinding {
        object_id: None,
        name: "pg_catalog.generate_series".into(),
        argument_types: vec![kind.into(); 3],
        builtin: true,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    }
}

#[test]
fn integer_series_keeps_distinct_two_and_three_argument_identities() {
    let overloads = builtin_table_function_overloads("generate_series", &[]);
    assert_eq!(overloads.len(), 4);
    for kind in [ColumnType::Integer, ColumnType::BigInteger] {
        for count in [2, 3] {
            let overload = overloads
                .iter()
                .find(|overload| overload.argument_types == vec![kind.clone(); count])
                .unwrap();
            assert_eq!(overload.default_arguments, 0);
            assert_eq!(overload.return_type, kind);
        }
    }
}

#[test]
fn legacy_series_upgrade_preserves_user_identities_and_explicit_steps() {
    for kind in ["integer", "bigint"] {
        let mut binding = Some(legacy_binding(kind));
        assert!(upgrade_legacy_table_function_binding(&mut binding, 2));
        assert_eq!(binding.as_ref().unwrap().argument_types, [kind, kind]);
        assert!(!upgrade_legacy_table_function_binding(&mut binding, 2));
        for count in [0, 1, 3, 4] {
            let mut unchanged = Some(legacy_binding(kind));
            let original = unchanged.clone();
            assert!(!upgrade_legacy_table_function_binding(
                &mut unchanged,
                count
            ));
            assert_eq!(unchanged, original);
        }
    }
    let mut user = legacy_binding("integer");
    user.builtin = false;
    user.object_id = Some([7; 16]);
    let mut other_schema = legacy_binding("integer");
    other_schema.name = "public.generate_series".into();
    let mut mixed = legacy_binding("integer");
    mixed.argument_types[2] = "bigint".into();
    for binding in [user, other_schema, mixed, legacy_binding("numeric")] {
        let mut unchanged = Some(binding);
        let original = unchanged.clone();
        assert!(!upgrade_legacy_table_function_binding(&mut unchanged, 2));
        assert_eq!(unchanged, original);
    }
}

#[test]
fn legacy_series_upgrade_reaches_stored_statement_and_view_sources() {
    let mut statement = crate::compile(
        "SELECT * FROM ROWS FROM(generate_series(1,3), generate_series(4,6)) AS s(a,b)",
    )
    .unwrap()
    .remove(0);
    let crate::ast::Statement::Select(select) = &mut statement else {
        panic!("query");
    };
    let Some(crate::ast::FromClause::FunctionGroup { functions, .. }) = &mut select.from else {
        panic!("range function group");
    };
    for function in functions {
        function.binding = Some(legacy_binding("integer"));
    }
    let mut plan = crate::plan::UnifiedPlan::lower(statement.clone());
    assert!(statement.upgrade_legacy_serialized_dispatches());
    assert!(!statement.upgrade_legacy_serialized_dispatches());
    let crate::plan::UnifiedPlan::Query(query) = &mut plan else {
        panic!("query");
    };
    assert!(crate::binding::view_dependencies::restoration::upgrade_legacy_view_dispatches(query));
    assert!(!crate::binding::view_dependencies::restoration::upgrade_legacy_view_dispatches(query));
}
