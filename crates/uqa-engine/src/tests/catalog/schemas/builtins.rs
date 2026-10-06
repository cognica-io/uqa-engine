//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_sql::catalog::security::BoundSchemaSecurity;

#[test]
fn builtin_namespace_initialization_is_atomic_and_preserves_existing_acl_identity() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE schema_reader; GRANT USAGE, CREATE ON SCHEMA information_schema TO schema_reader");
        let expected = first.durable.schemas.read()["information_schema"].clone();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.catalog.drop_schema("ag_catalog").unwrap();
        raw.catalog.drop_schema("pg_catalog").unwrap();
        raw.catalog
            .delete_metadata("sql_builtin_schema_catalog_initialized")
            .unwrap();
        let before = raw.catalog.load_schema_rows().unwrap();
        let Err(error) = first.new_session() else {
            panic!("secondary restoration must not initialize missing built-in schemas");
        };
        assert!(
            error.to_string().contains("initial catalog migration"),
            "{error}"
        );
        assert_eq!(raw.catalog.load_schema_rows().unwrap(), before);
        raw.catalog.set_metadata("sql_functions_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("invalid later catalog metadata must reject the complete restoration");
        };
        assert!(error.to_string().contains("EOF"), "{error}");
        assert_eq!(raw.catalog.load_schema_rows().unwrap(), before);
        assert_eq!(
            raw.catalog
                .get_metadata("sql_builtin_schema_catalog_initialized")
                .unwrap(),
            None
        );
        raw.catalog.delete_metadata("sql_functions_json").unwrap();
        let restored = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        assert_eq!(
            restored.durable.schemas.read()["information_schema"],
            expected
        );
        for name in ["pg_catalog", "ag_catalog"] {
            assert_eq!(
                restored.durable.schemas.read().get(name),
                BoundSchemaSecurity::builtin(name).as_ref()
            );
        }
        sql(&restored, "SET ROLE schema_reader; CREATE TABLE information_schema.authorized(n integer); RESET ROLE; CREATE TABLE ag_catalog.restored(n integer)");
        let rows = raw.catalog.load_schema_rows().unwrap();
        drop(restored);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(
            reopened.durable.schemas.read()["information_schema"],
            expected
        );
        assert_eq!(raw.catalog.load_schema_rows().unwrap(), rows);
        sql(
            &reopened,
            "SELECT * FROM information_schema.authorized; SELECT * FROM ag_catalog.restored",
        );
    }
}
