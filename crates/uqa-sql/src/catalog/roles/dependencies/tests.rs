//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod fixtures;
use fixtures::Catalog;

#[test]
fn table_dependencies_visit_requested_roles_first_and_read_security_lazily() {
    let mut catalog = Catalog::new();
    catalog.table("a", "second");
    catalog.table("b", "first");
    catalog.table("c", "first");
    let error =
        ensure_roles_have_no_object_dependencies(&catalog, &["first".into(), "second".into()])
            .unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "2BP01" && message == "role \"first\" cannot be dropped because some objects depend on it: table public.b")
    );
    assert_eq!(
        *catalog.events.borrow(),
        [
            "read database",
            "release database",
            "read schemas",
            "release schemas",
            "read tables",
            "security a",
            "security b",
            "release tables"
        ]
    );
}

#[test]
fn an_earlier_catalog_dependency_prevents_reading_later_registries() {
    let mut catalog = Catalog::new();
    catalog.database.role_owner = "second".into();
    catalog.schemas.insert(
        "owned".into(),
        SchemaSecurity {
            role_owner: "first".into(),
            acl: None,
        },
    );
    let error =
        ensure_roles_have_no_object_dependencies(&catalog, &["first".into(), "second".into()])
            .unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { message, .. } if message == "role \"second\" cannot be dropped because some objects depend on it: database uqa")
    );
    assert_eq!(
        *catalog.events.borrow(),
        ["read database", "release database"]
    );
}

#[test]
fn dependency_readers_release_each_registry_before_the_next_is_acquired() {
    let catalog = Catalog::new();
    ensure_roles_have_no_object_dependencies(&catalog, &["unreferenced".into()]).unwrap();
    assert_eq!(
        *catalog.events.borrow(),
        [
            "read database",
            "release database",
            "read schemas",
            "release schemas",
            "read tables",
            "release tables",
            "read views",
            "release views",
            "read foreign",
            "release foreign",
            "read sequences",
            "release sequences",
            "read routines",
            "release routines"
        ]
    );
}
