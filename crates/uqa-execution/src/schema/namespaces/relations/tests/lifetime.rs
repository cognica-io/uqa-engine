//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::row_locks::shared_objects::SharedCatalogLock;
use crate::schema::namespaces::identity::SCHEMA_CATALOG_CLASS_ID;
use uqa_core::catalog_schema::SchemaTupleIdentity;

fn peer_acquires(fixture: &Fixture, oid: i64) -> bool {
    let key = fixture.locks.shared_catalog_key(SharedCatalogLock::Object {
        class_id: SCHEMA_CATALOG_CLASS_ID,
        oid: oid as u32,
    });
    fixture
        .locks
        .try_acquire_relation(
            2,
            key,
            RelationLockMode::AccessExclusive,
            0,
            &fixture.cancellation,
        )
        .unwrap()
}

#[test]
fn relation_creation_keeps_the_selected_namespace_but_nonrelation_resolution_does_not() {
    for relation in [false, true] {
        let fixture = Fixture::new();
        let security = BoundSchemaSecurity::bootstrap("tenant");
        let oid = security.tuple.unwrap().oid;
        fixture
            .schemas
            .borrow_mut()
            .insert("tenant".into(), security);
        let context = fixture.context();
        let name = if relation {
            context.persistent_relation_name("docs")
        } else {
            context.persistent_name("docs")
        };
        assert_eq!(name.unwrap(), "tenant.docs");
        assert_eq!(peer_acquires(&fixture, oid), !relation);
    }
}

#[test]
fn creation_rebinds_search_path_and_releases_the_deleted_namespace() {
    let mut fixture = Fixture::new();
    fixture.path.push("fallback".into());
    let old = BoundSchemaSecurity::bootstrap("tenant");
    let new = BoundSchemaSecurity::bootstrap("fallback");
    let old_oid = old.tuple.unwrap().oid;
    let new_oid = new.tuple.unwrap().oid;
    fixture
        .schemas
        .borrow_mut()
        .extend([("tenant".into(), old), ("fallback".into(), new.clone())]);
    fixture
        .refreshes
        .borrow_mut()
        .push_back(BTreeMap::from([("fallback".into(), new)]));
    assert_eq!(
        fixture.context().persistent_relation_name("docs").unwrap(),
        "fallback.docs"
    );
    assert!(peer_acquires(&fixture, old_oid));
    assert!(!peer_acquires(&fixture, new_oid));
}

#[test]
fn creation_rebinds_recreated_identity_even_when_its_oid_is_reused() {
    let fixture = Fixture::new();
    let before = BoundSchemaSecurity::bootstrap("tenant");
    let mut after = before.clone();
    after.tuple = Some(SchemaTupleIdentity {
        object_id: [7; 16],
        revision: [8; 16],
        ..before.tuple.unwrap()
    });
    fixture
        .schemas
        .borrow_mut()
        .insert("tenant".into(), before.clone());
    fixture
        .refreshes
        .borrow_mut()
        .push_back(BTreeMap::from([("tenant".into(), after)]));
    assert_eq!(
        fixture.context().persistent_relation_name("docs").unwrap(),
        "tenant.docs"
    );
    assert_eq!(
        fixture
            .events
            .borrow()
            .iter()
            .filter(|event| **event == "namespace_lock")
            .count(),
        2
    );
    assert!(!peer_acquires(&fixture, before.tuple.unwrap().oid));
}

#[test]
fn creation_checks_authority_before_waiting_and_again_after_replacement() {
    for revoked_after_wait in [false, true] {
        let mut fixture = Fixture::new();
        let mut role = RoleDefinition::bootstrap();
        role.name = "reader".into();
        role.oid = 42_001;
        role.object_id = [4; 16];
        role.attributes.clear();
        fixture.roles.insert(role.name.clone(), role);
        fixture.user = "reader".into();
        let denied = BoundSchemaSecurity::bootstrap("tenant");
        let mut allowed = BoundSchemaSecurity::with_public_privileges(true);
        allowed.tuple = denied.tuple;
        fixture.schemas.borrow_mut().insert(
            "tenant".into(),
            if revoked_after_wait {
                allowed
            } else {
                denied.clone()
            },
        );
        fixture
            .refreshes
            .borrow_mut()
            .push_back(BTreeMap::from([("tenant".into(), denied.clone())]));
        let error = fixture
            .context()
            .persistent_relation_name("tenant.docs")
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
        assert_eq!(
            fixture.events.borrow().contains(&"namespace_lock"),
            revoked_after_wait
        );
        assert!(peer_acquires(&fixture, denied.tuple.unwrap().oid));
    }
}

#[test]
fn direct_api_creation_retains_the_same_namespace_lifetime() {
    let fixture = Fixture::new();
    let security = BoundSchemaSecurity::bootstrap("tenant");
    let oid = security.tuple.unwrap().oid;
    fixture
        .schemas
        .borrow_mut()
        .insert("tenant".into(), security);
    assert_eq!(fixture.context().api_name("docs").unwrap(), "tenant.docs");
    assert!(!peer_acquires(&fixture, oid));
}

#[test]
fn direct_api_creation_preserves_typed_namespace_lock_errors() {
    let mut fixture = Fixture::new();
    fixture.fail = Some("namespace_lock");
    let security = BoundSchemaSecurity::bootstrap("tenant");
    let oid = security.tuple.unwrap().oid;
    fixture
        .schemas
        .borrow_mut()
        .insert("tenant".into(), security);
    let error = fixture.context().api_name("docs").unwrap_err();
    let error = uqa_sql::catalog::errors::storage_error("CREATE TABLE", &error);
    assert_eq!(error.sqlstate(), Some("55P03"));
    assert!(peer_acquires(&fixture, oid));
}
