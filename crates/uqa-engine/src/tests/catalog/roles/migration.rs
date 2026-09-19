//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role record conversion shares initial restoration's rollback boundary.

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use std::sync::Arc;

#[test]
fn role_record_migration_preserves_legacy_oids_and_rolls_back_on_later_restore_failure() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE legacy LOGIN CONNECTION LIMIT 7; CREATE ROLE member; GRANT legacy TO member");
        let expected = first.durable.roles.snapshot();
        let memberships = first.durable.role_memberships.snapshot();
        let legacy = serde_json::to_string(expected.as_ref()).unwrap();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.backend.begin_transaction().unwrap();
        for prefix in ["uqa.sql.role.v1:", "uqa.sql.role_oid.v1:"] {
            for (key, _) in raw.catalog.metadata_with_prefix(prefix).unwrap() {
                raw.catalog.delete_metadata(&key).unwrap();
            }
        }
        raw.catalog.set_metadata("sql_roles_json", &legacy).unwrap();
        raw.backend.commit_transaction().unwrap();
        let Err(error) = first.new_session() else {
            panic!("secondary role restoration must not migrate legacy records");
        };
        assert!(
            error.to_string().contains("initial-open record migration"),
            "{error}"
        );
        raw.catalog.set_metadata("sql_functions_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("initial restoration accepted malformed function metadata");
        };
        assert!(error.to_string().contains("EOF"), "{error}");
        assert_eq!(
            raw.catalog.get_metadata("sql_roles_json").unwrap(),
            Some(legacy)
        );
        for prefix in ["uqa.sql.role.v1:", "uqa.sql.role_oid.v1:"] {
            assert!(raw.catalog.metadata_with_prefix(prefix).unwrap().is_empty());
        }
        raw.catalog.delete_metadata("sql_functions_json").unwrap();
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(*reopened.durable.roles.read(), *expected);
        assert_eq!(*reopened.durable.role_memberships.read(), *memberships);
        assert_eq!(
            raw.catalog
                .get_metadata("sql_roles_json")
                .unwrap()
                .as_deref(),
            Some(r#"{"role_catalog_format":3}"#)
        );
    }
}

#[test]
fn role_incarnation_migration_rolls_back_with_initial_catalog_restoration() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE legacy; CREATE ROLE member; GRANT legacy TO member",
        );
        let expected = first.durable.roles.snapshot();
        let memberships = first.durable.role_memberships.snapshot();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.backend.begin_transaction().unwrap();
        let mut legacy = Vec::new();
        for (key, json) in raw
            .catalog
            .metadata_with_prefix("uqa.sql.role.v1:")
            .unwrap()
        {
            let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
            value.as_object_mut().unwrap().remove("object_id");
            let json = value.to_string();
            raw.catalog.set_metadata(&key, &json).unwrap();
            legacy.push((key, json));
        }
        raw.catalog
            .set_metadata("sql_roles_json", r#"{"role_catalog_format":1}"#)
            .unwrap();
        raw.backend.commit_transaction().unwrap();
        assert!(first
            .new_session()
            .err()
            .unwrap()
            .to_string()
            .contains("initial-open record migration"));
        raw.catalog.set_metadata("sql_functions_json", "{").unwrap();
        drop(second);
        drop(first);
        assert!(Engine::from_persistent_provider(Arc::clone(&factory))
            .err()
            .unwrap()
            .to_string()
            .contains("EOF"));
        assert_eq!(
            raw.catalog
                .get_metadata("sql_roles_json")
                .unwrap()
                .as_deref(),
            Some(r#"{"role_catalog_format":1}"#)
        );
        assert_eq!(
            raw.catalog
                .metadata_with_prefix("uqa.sql.role.v1:")
                .unwrap(),
            legacy
        );
        raw.catalog.delete_metadata("sql_functions_json").unwrap();
        let reopened = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        for (name, role) in reopened.durable.roles.read().iter() {
            assert_eq!(role.oid, expected[name].oid);
            assert_ne!(role.object_id, [0; 16]);
        }
        assert_eq!(*reopened.durable.role_memberships.read(), *memberships);
        let identities = reopened.durable.roles.snapshot();
        drop(reopened);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(*reopened.durable.roles.read(), *identities);
    }
}

#[test]
fn role_tuple_revision_conversion_rolls_back_with_later_catalog_restoration() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE legacy LOGIN; CREATE ROLE member; GRANT legacy TO member",
        );
        let expected = first.durable.roles.snapshot();
        let memberships = first.durable.role_memberships.snapshot();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.backend.begin_transaction().unwrap();
        let mut legacy = Vec::new();
        for (key, json) in raw
            .catalog
            .metadata_with_prefix("uqa.sql.role.v1:")
            .unwrap()
        {
            let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
            value.as_object_mut().unwrap().remove("revision");
            let json = value.to_string();
            raw.catalog.set_metadata(&key, &json).unwrap();
            legacy.push((key, json));
        }
        raw.catalog
            .set_metadata("sql_roles_json", r#"{"role_catalog_format":2}"#)
            .unwrap();
        raw.backend.commit_transaction().unwrap();
        assert!(first.new_session().is_err());
        let routines = raw
            .catalog
            .get_metadata("sql_functions_json")
            .unwrap()
            .unwrap();
        raw.catalog.set_metadata("sql_functions_json", "{").unwrap();
        drop(second);
        drop(first);
        assert!(Engine::from_persistent_provider(Arc::clone(&factory)).is_err());
        assert_eq!(
            raw.catalog
                .get_metadata("sql_roles_json")
                .unwrap()
                .as_deref(),
            Some(r#"{"role_catalog_format":2}"#)
        );
        assert_eq!(
            raw.catalog
                .metadata_with_prefix("uqa.sql.role.v1:")
                .unwrap(),
            legacy
        );
        raw.catalog
            .set_metadata("sql_functions_json", &routines)
            .unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(*restored.durable.roles.read(), *expected);
        assert_eq!(*restored.durable.role_memberships.read(), *memberships);
        sql(&restored, "ALTER ROLE legacy NOLOGIN");
        assert_eq!(restored.durable.roles.read()["legacy"].revision, 2);
    }
}
