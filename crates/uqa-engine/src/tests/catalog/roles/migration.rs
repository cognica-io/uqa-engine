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
            Some(r#"{"role_catalog_format":1}"#)
        );
    }
}
