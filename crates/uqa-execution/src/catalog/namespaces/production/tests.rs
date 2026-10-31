//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::tests::{catalog, resolution};
use super::*;
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::catalog::security::BoundSchemaSecurity;

#[test]
fn controlled_namespace_selection_preserves_implicit_order_and_privilege_errors() {
    let mut snapshot = catalog().snapshot().clone();
    let mut invalid = BoundSchemaSecurity::with_public_privileges(true);
    invalid.acl.as_mut().unwrap().last_mut().unwrap().grantor = RoleIdentity {
        oid: 500,
        object_id: [50; 16],
    };
    Arc::make_mut(&mut snapshot.definitions.schemas).insert("invalid".into(), invalid);
    let catalog = CatalogReadView::new(snapshot);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for allocated in [false, true] {
        for implicit in [false, true] {
            for role in ["uqa", "reader", "missing"] {
                let mut path = resolution(&[
                    "invalid",
                    "missing",
                    "pg_temp",
                    "g",
                    "private",
                    "public",
                    "g",
                    "pg_catalog",
                ]);
                path.temporary_namespace_allocated = allocated;
                let expected = super::super::current_schema_names(&catalog, &path, role, implicit);
                let names =
                    current_schema_names_with_control(&catalog, &path, role, implicit, &control)
                        .unwrap();
                assert_eq!(*names, expected);
                let bytes = names.capacity() * size_of::<String>()
                    + names.iter().map(String::capacity).sum::<usize>();
                assert_eq!(names.reserved_bytes(), bytes);
                assert_eq!(budget.used(), bytes);
                drop(names);
                assert_eq!(budget.used(), 0);
            }
        }
    }
    assert!(namespace_schema_exists(&catalog, "pg_temp_1", "pg_temp_1"));
    assert!(namespace_schema_exists(
        &catalog,
        "pg_temp_1",
        "pg_toast_temp_1"
    ));
    assert!(!namespace_schema_exists(&catalog, "", "pg_temp_1"));
    assert!(!namespace_schema_exists(&catalog, "", "pg_toast_temp_1"));
    assert!(!namespace_schema_exists(&catalog, "", ""));
    for (temporary, schema, role, expected) in [
        ("pg_temp_1", "pg_temp_1", "reader", true),
        ("pg_temp_1", "pg_toast_temp_1", "reader", false),
        ("pg_temp_1", "pg_toast_temp_1", "uqa", true),
        ("", "pg_temp_1", "uqa", false),
        ("pg_temp_1", "invalid", "uqa", false),
    ] {
        assert_eq!(
            schema_has_privilege_with_control(
                &catalog,
                temporary,
                schema,
                role,
                SchemaAclPrivilege::Usage,
                &control
            )
            .unwrap(),
            expected,
            "{temporary}/{schema}/{role}"
        );
    }
    assert!(!namespace_schema_exists(&catalog, "pg_temp_1", "missing"));
    for schema in ["missing", "private"] {
        assert_eq!(
            require_schema_privilege_with_control(
                &catalog,
                "pg_temp_1",
                schema,
                "reader",
                SchemaAclPrivilege::Usage,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("42501")
        );
    }
    assert!(matches!(
        require_schema_privilege_with_control(
            &catalog,
            "pg_temp_1",
            "invalid",
            "uqa",
            SchemaAclPrivilege::Usage,
            &control
        ),
        Err(SQLError::Internal(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_namespace_rejection_and_cancellation_release_partial_results() {
    let name = "long_namespace_".repeat(64 * 1024);
    let mut snapshot = catalog().snapshot().clone();
    Arc::make_mut(&mut snapshot.definitions.schemas).insert(
        name.clone(),
        BoundSchemaSecurity::with_public_privileges(true),
    );
    let catalog = CatalogReadView::new(snapshot);
    let path = resolution(&["public", &name]);
    let budget = MemoryBudget::new(512);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(7).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error = current_schema_names_with_control(&catalog, &path, "reader", true, &control)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), 7);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            current_schema_names_with_control(&catalog, &path, "reader", true, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 7);
        token.reset();
    }
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
