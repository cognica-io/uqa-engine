//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};

#[test]
fn memory_catalog_authority_preserves_rollback_drop_and_unread_source_closure() {
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE ROLE reader; CREATE SCHEMA saved; CREATE SEQUENCE saved.ids; GRANT USAGE ON SCHEMA saved TO reader WITH GRANT OPTION; GRANT USAGE ON SEQUENCE saved.ids TO reader; GRANT CREATE ON DATABASE uqa TO reader");
        let database = engine.durable.database_security.snapshot();
        let schemas = engine.durable.schemas.snapshot();
        let sequences = engine.durable.sequence_security.snapshot();
        let database_key = engine.durable.database_security.key().unwrap();
        let schemas_key = engine.durable.schemas.key().unwrap();
        let sequences_key = engine.durable.sequence_security.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine, "BEGIN; REVOKE USAGE ON SCHEMA saved FROM reader; REVOKE USAGE ON SEQUENCE saved.ids FROM reader; REVOKE CREATE ON DATABASE uqa FROM reader; SAVEPOINT changed; DROP SEQUENCE saved.ids; ROLLBACK TO changed; ROLLBACK");
            assert_eq!(*engine.durable.database_security.read(), *database);
            assert_eq!(*engine.durable.schemas.read(), *schemas);
            assert_eq!(*engine.durable.sequence_security.read(), *sequences);
            sql(&engine, "REVOKE CREATE ON DATABASE uqa FROM reader; DROP SEQUENCE saved.ids; DROP SCHEMA saved; CREATE SCHEMA later");
        }
        drop(engine);
        let saved_database = database_key.retained(&cut).unwrap().unwrap();
        let saved_schemas = schemas_key.retained(&cut).unwrap().unwrap();
        let saved_sequences = sequences_key.retained(&cut).unwrap().unwrap();
        drop(cut);
        assert_eq!(
            saved_database.get().unwrap().as_ref(),
            database.as_ref().as_ref()
        );
        assert!(saved_schemas.get().unwrap().iter().eq(schemas.iter()));
        assert!(saved_sequences
            .get()
            .unwrap()
            .iter()
            .map(|(k, v)| (k, v))
            .eq(sequences.iter()));
        control.cancellation().cancel();
        assert!(saved_schemas.get().is_err());
        drop((saved_database, saved_schemas, saved_sequences));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn failed_catalog_authority_capture_does_not_change_grant_publication() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE ROLE reader; GRANT USAGE ON SCHEMA public TO reader",
    );
    let key = engine.durable.schemas.key().unwrap();
    let original = engine.durable.schemas.snapshot();
    let small = StorageReadControl::with_limit(512);
    let good = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    sql(&engine, "REVOKE USAGE ON SCHEMA public FROM reader");
    assert!(failed.check().is_err());
    assert_ne!(*engine.durable.schemas.read(), *original);
    let saved = key.retained(&cut).unwrap().unwrap();
    assert!(saved.get().unwrap().iter().eq(original.iter()));
    drop((cut, failed, saved, engine));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
