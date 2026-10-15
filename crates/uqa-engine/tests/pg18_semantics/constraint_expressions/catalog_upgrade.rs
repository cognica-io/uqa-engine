//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing definitions acquire physical rows atomically during initial restoration.

use std::sync::Arc;
use uqa_engine::Engine;
use uqa_storage::PersistentStorageProvider;

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn physical_constraint_initialization_preserves_all_definition_owners(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog-upgrade.db");
    let provider: Arc<dyn PersistentStorageProvider> = match provider {
        0 => Arc::new(uqa_storage_sqlite::SQLiteStorageProvider::new(
            uqa_storage_sqlite::ManagedConnection::open(&path).unwrap(),
        )),
        1 => Arc::new(uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap()),
        2 => Arc::new(uqa_storage_redb::RedbStorage::open(&path).unwrap()),
        _ => unreachable!(),
    };
    let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
    for sql in [
        "CREATE TABLE upgrade_relation(n integer CONSTRAINT upgrade_not_null NOT NULL,CONSTRAINT upgrade_primary PRIMARY KEY(n),CONSTRAINT upgrade_check CHECK(n>0))",
        "INSERT INTO upgrade_relation VALUES(3)",
        "CREATE DOMAIN upgrade_domain AS integer CONSTRAINT upgrade_domain_check CHECK(VALUE>0)",
        "CREATE FOREIGN DATA WRAPPER memory",
        "CREATE SERVER upgrade_server FOREIGN DATA WRAPPER memory",
        "CREATE FOREIGN TABLE upgrade_foreign(n integer,CONSTRAINT upgrade_foreign_check CHECK(n>0)) SERVER upgrade_server",
        "CREATE FUNCTION upgrade_trigger_function() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "CREATE CONSTRAINT TRIGGER upgrade_trigger AFTER INSERT ON upgrade_relation FOR EACH ROW EXECUTE FUNCTION upgrade_trigger_function()",
    ] { engine.sql(sql, &[]).unwrap(); }
    let query = "SELECT conname,contype,conkey::text,conbin IS NOT NULL AS has_node FROM pg_constraint WHERE conname LIKE 'upgrade_%' ORDER BY conname";
    let expected = engine.sql(query, &[]).unwrap().rows;
    assert_eq!(expected.len(), 6);
    drop(engine);

    // Model the preceding format's absence of physical catalog records, retaining its definitions and row data.
    let session = provider.open_session().unwrap();
    session.backend.begin_transaction().unwrap();
    for prefix in [
        "uqa.sql.catalog.tuple.v1:",
        "uqa.sql.catalog.current.v1:",
        "uqa.sql.catalog.constraint.",
    ] {
        for (key, _) in session.catalog.metadata_with_prefix(prefix).unwrap() {
            session.catalog.delete_metadata(&key).unwrap();
        }
    }
    session.backend.commit_transaction().unwrap();
    drop(session);

    for _ in 0..2 {
        let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
        assert_eq!(engine.sql(query, &[]).unwrap().rows, expected);
        assert_eq!(
            engine
                .sql("SELECT n FROM upgrade_relation", &[])
                .unwrap()
                .rows[0]["n"],
            uqa_core::Value::Int(3)
        );
        let session = provider.open_session().unwrap();
        assert_eq!(
            session
                .catalog
                .get_metadata("uqa.sql.catalog.constraint.format")
                .unwrap()
                .as_deref(),
            Some("1")
        );
        assert!(session
            .catalog
            .metadata_with_prefix("uqa.sql.catalog.constraint.pending:")
            .unwrap()
            .is_empty());
    }
}
