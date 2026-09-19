//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn relocation_uses_an_explicit_namespace_despite_the_creation_search_path() {
    let fixture = Fixture::new();
    fixture
        .schemas
        .borrow_mut()
        .insert("public".into(), BoundSchemaSecurity::bootstrap("public"));
    let target = RelationIdentity::new("public", "ids");
    assert_eq!(
        fixture.context().relocation_target(&target).unwrap(),
        target
    );
    assert!(fixture.events.borrow().contains(&"namespace_lock"));
}

#[test]
fn relocation_allocates_only_the_temp_alias_and_checks_temp_authority_before_locking() {
    let mut fixture = Fixture::new();
    let alias = RelationIdentity::new("pg_temp", "ids");
    let target = RelationIdentity::new("pg_temp_42", "ids");
    assert_eq!(
        fixture
            .context()
            .relocation_target(&target)
            .unwrap_err()
            .sqlstate(),
        Some("3F000")
    );
    assert!(!fixture.allocated.get());
    fixture.user = "guest".into();
    fixture.database.acl = Some(Vec::new());
    let error = fixture.context().relocation_target(&alias).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert!(error
        .to_string()
        .contains("create temporary tables in database"));
    assert!(!fixture.allocated.get());
    assert!(!fixture.events.borrow().contains(&"namespace_lock"));
    fixture.user = "uqa".into();
    assert_eq!(fixture.context().relocation_target(&alias).unwrap(), target);
    assert!(fixture.allocated.get());
    fixture.user = "guest".into();
    fixture.events.borrow_mut().clear();
    let error = fixture.context().relocation_target(&alias).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert!(error
        .to_string()
        .contains("permission denied for schema pg_temp_42"));
    assert!(!fixture.events.borrow().contains(&"namespace_lock"));
}
