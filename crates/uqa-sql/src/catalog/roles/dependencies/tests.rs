//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod fixtures;
mod temporary;
use fixtures::Catalog;

#[test]
fn domain_dependencies_keep_the_owner_incarnation_after_its_name_is_reused() {
    let mut catalog = Catalog::new();
    let crate::Statement::CreateDomain(definition) =
        crate::compile("CREATE DOMAIN public.owned AS integer")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    catalog.domains.insert(
        "public.owned".into(),
        crate::catalog::domain::StoredDomain {
            object_id: [1; 16],
            oid: crate::catalog::domain::domain_object_oid(&[1; 16]),
            identity: RelationIdentity::new("public", "owned"),
            owner: catalog.roles["first"].identity(),
            definition,
        },
    );
    let mut original = catalog.roles.remove("first").unwrap();
    original.name = "renamed".into();
    catalog.roles.insert("renamed".into(), original);
    let mut replacement = catalog.roles["second"].clone();
    replacement.name = "first".into();
    replacement.oid = 55_555;
    replacement.object_id = [55; 16];
    catalog.roles.insert("first".into(), replacement);
    ensure_roles_have_no_object_dependencies(&catalog, &["first".into()], &catalog.roles).unwrap();
    assert_eq!(
        ensure_roles_have_no_object_dependencies(&catalog, &["renamed".into()], &catalog.roles)
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    catalog.domains.clear();
    ensure_roles_have_no_object_dependencies(&catalog, &["renamed".into()], &catalog.roles)
        .unwrap();
}

#[test]
fn table_dependencies_visit_requested_roles_first_and_read_security_lazily() {
    let mut catalog = Catalog::new();
    catalog.table("a", catalog.roles["second"].identity());
    catalog.table("b", catalog.roles["first"].identity());
    catalog.table("c", catalog.roles["first"].identity());
    let error = ensure_roles_have_no_object_dependencies(
        &catalog,
        &["first".into(), "second".into()],
        &catalog.roles,
    )
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
    catalog.database.role_owner = catalog.roles["second"].identity();
    catalog.schemas.insert(
        "owned".into(),
        BoundSchemaSecurity {
            tuple: None,
            role_owner: catalog.roles["first"].identity(),
            acl: None,
        },
    );
    let error = ensure_roles_have_no_object_dependencies(
        &catalog,
        &["first".into(), "second".into()],
        &catalog.roles,
    )
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
    ensure_roles_have_no_object_dependencies(&catalog, &["unreferenced".into()], &catalog.roles)
        .unwrap();
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
            "read domains",
            "release domains",
            "read routines",
            "release routines"
        ]
    );
}
