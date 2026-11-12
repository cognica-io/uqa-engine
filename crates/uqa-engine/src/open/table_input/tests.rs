//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{tests::providers::Provider, Engine};
use uqa_core::Value;
use uqa_execution::catalog::security::relation_restoration::TableDeclarationInput;

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

fn declarations(engine: &Engine, control: &StorageReadControl) -> PreparedTableDeclarations {
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let mut source = catalog.begin_table_load(LIMITS, control).unwrap();
    let tables = loop {
        if let Some(tables) = source.advance(1).unwrap() {
            break tables;
        }
    };
    let roles = engine.durable.roles.read().clone();
    let mut prepared = TableDeclarationInput::new(tables, roles, control).unwrap();
    loop {
        if let Some(tables) = prepared.advance(1).unwrap() {
            return tables;
        }
    }
}

fn input(engine: &Engine, control: &StorageReadControl) -> PersistentTableInput {
    PersistentTableInput::from_catalog(
        engine.storage.catalog.as_ref().unwrap().clone(),
        engine.storage.backend.as_ref().unwrap().clone(),
        engine.durable.roles.read().clone(),
        LIMITS,
        control,
    )
    .unwrap()
}

fn finish(input: &mut PersistentTableInput) -> StorageBackendResult<TableRegistry> {
    for _ in 0..65536 {
        if let Some(tables) = input.advance(1)? {
            return Ok(tables);
        }
    }
    panic!("persistent table hydration did not finish");
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_table_hydration_preserves_native_state_and_runtime_writes(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut engine = provider.open(&directory.path().join("tables.db"));
    engine.sql("CREATE TABLE kept(id integer PRIMARY KEY, body text DEFAULT 'original'); INSERT INTO kept VALUES(1,'first'),(2,'second'); ANALYZE kept; CREATE TABLE tail(value text)", &[]).unwrap();
    let old = engine.try_table("kept").unwrap().unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let mut input = input(&engine, &control);
    let registry = finish(&mut input).unwrap();
    drop(input);
    let relation = uqa_core::RelationIdentity::new("public", "kept");
    let table = registry.read()[&relation].clone();
    assert_eq!(table.object_id(), old.object_id());
    assert_eq!(
        *table.storage_generation.read(),
        *old.storage_generation.read()
    );
    assert_eq!(*table.security.read(), *old.security.read());
    assert_eq!(
        serde_json::to_vec(&*table.columns.read()).unwrap(),
        serde_json::to_vec(&*old.columns.read()).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&*table.key_constraints.read()).unwrap(),
        serde_json::to_vec(&*old.key_constraints.read()).unwrap()
    );
    assert_eq!(
        *table.maps_integer_keys.read(),
        *old.maps_integer_keys.read()
    );
    assert_eq!(
        *table.next_id.read(),
        u128::from(old.document_store.read().max_doc_id().unwrap()) + 1
    );
    assert!(*table.column_stats_loaded.read());
    assert!(!table.column_stats.read().is_empty());
    assert_eq!(
        *table.statistics_maintenance.read(),
        StatisticsMaintenance::default()
    );
    drop(old);
    engine.storage.tables = Arc::new(registry);
    let held = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    engine.sql("BEGIN; ALTER TABLE kept RENAME TO moved; INSERT INTO moved VALUES(3,'third'); SAVEPOINT preserved; DROP TABLE moved; ROLLBACK TO preserved; COMMIT", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM moved", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(3)
    );
    drop((table, engine, held));
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_table_hydration_preserves_dirty_statistics_short_circuit(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("dirty.db"));
    engine
        .sql(
            "CREATE TABLE kept(id integer); INSERT INTO kept VALUES(1)",
            &[],
        )
        .unwrap();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let key = format!("{}public.kept", StatisticsMaintenance::KEY_PREFIX);
    catalog
        .set_metadata(&key, "invalid maintenance JSON")
        .unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut hydration = input(&engine, &control);
    let tables = finish(&mut hydration).unwrap();
    let table = tables.read()[&uqa_core::RelationIdentity::new("public", "kept")].clone();
    assert!(*table.column_stats_dirty.read());
    assert_eq!(
        *table.statistics_maintenance.read(),
        StatisticsMaintenance::default()
    );
    drop((table, tables, hydration));
    assert_eq!(control.memory().used(), 0);
    catalog.delete_metadata(&key).unwrap();
    engine.sql("ANALYZE kept", &[]).unwrap();
    catalog
        .set_metadata(&key, "invalid maintenance JSON")
        .unwrap();
    let mut hydration = input(&engine, &control);
    assert!(finish(&mut hydration).is_err());
    assert!(hydration.advance(1).is_err());
    drop(hydration);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn persistent_table_hydration_retires_every_pending_prefix_and_input() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Provider::SQLiteKeyValue.open(&directory.path().join("retirement.db"));
    engine.sql("CREATE TABLE first(id integer); INSERT INTO first VALUES(1); CREATE TABLE second(body text)", &[]).unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut baseline = input(&engine, &control);
    let mut pending = 0;
    while baseline.advance(1).unwrap().is_none() {
        pending += 1;
        assert!(pending < 65536);
    }
    drop(baseline);
    assert!(pending > 10);
    assert_eq!(control.memory().used(), 0);
    for stop in 0..=pending {
        for cancel in [false, true] {
            let control = StorageReadControl::with_limit(8 << 20);
            let mut hydration = input(&engine, &control);
            for _ in 0..stop {
                assert!(hydration.advance(1).unwrap().is_none());
            }
            if cancel {
                control.cancellation().cancel();
                let error = hydration.advance(1).err().unwrap();
                assert_eq!(
                    uqa_execution::storage_errors::storage_error("restore table", &error)
                        .sqlstate(),
                    Some("57014")
                );
                assert!(hydration.advance(1).is_err());
            }
            drop(hydration);
            assert_eq!(control.memory().used(), 0, "stop {stop}, cancel {cancel}");
        }
    }
}

#[test]
fn persistent_table_hydration_rejects_wrong_or_exhausted_input_allowances() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Provider::SQLiteKeyValue.open(&directory.path().join("admission.db"));
    engine.sql("CREATE TABLE kept(id integer)", &[]).unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let other = StorageReadControl::with_limit(8 << 20);
    let declarations = declarations(&engine, &control);
    assert!(PersistentTableInput::new(
        engine.storage.catalog.as_ref().unwrap().clone(),
        engine.storage.backend.as_ref().unwrap().clone(),
        declarations,
        KeyConstraintNames::load(&**engine.storage.catalog.as_ref().unwrap()).unwrap(),
        LIMITS,
        &other,
    )
    .is_err());
    assert_eq!((control.memory().used(), other.memory().used()), (0, 0));
    let mut hydration = input(&engine, &control);
    let held = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    let error = finish(&mut hydration).err().unwrap();
    assert_eq!(
        uqa_execution::storage_errors::storage_error("restore table", &error).sqlstate(),
        Some("53200")
    );
    assert!(hydration.advance(1).is_err());
    assert_eq!(control.memory().used(), held.bytes());
    drop((hydration, held));
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_table_hydration_keeps_legacy_reservations_and_mapping_markers(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("identities.db"));
    engine
        .sql(
            "CREATE TABLE kept(id integer PRIMARY KEY); INSERT INTO kept VALUES(7)",
            &[],
        )
        .unwrap();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let key = identifiers::legacy_document_id_metadata_key("public.kept");
    catalog
        .set_metadata(&key, "invalid reservation for an unrelated column")
        .unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let ordinary = finish(&mut input(&engine, &control)).unwrap();
    let relation = uqa_core::RelationIdentity::new("public", "kept");
    assert_eq!(*ordinary.read()[&relation].next_id.read(), 8);
    drop(ordinary);
    let mut schema = catalog.load_tables().unwrap().remove(0);
    let mut columns: Vec<uqa_sql::ast::ColumnDef> =
        serde_json::from_str(&schema.columns_json).unwrap();
    columns[0].auto_increment = Some(uqa_sql::ast::AutoIncrement::legacy());
    schema.columns_json = serde_json::to_string(&columns).unwrap();
    let reservation = "+00018446744073709551616";
    catalog.set_metadata(&key, reservation).unwrap();
    catalog.save_table(&schema).unwrap();
    // Durable table publication consumes the legacy value into its allocator. Seed the stored legacy field after publication so hydration itself must preserve it.
    catalog.set_metadata(&key, reservation).unwrap();
    catalog
        .set_metadata(identifiers::key_mapping::VERIFIED_KEY, "")
        .unwrap();
    let mapping = identifiers::key_mapping::indexed_key(schema.object_id);
    for (marker, expected) in [("", true), ("indexed", false)] {
        catalog.set_metadata(&mapping, marker).unwrap();
        let restored = finish(&mut input(&engine, &control)).unwrap();
        let table = restored.read()[&relation].clone();
        assert_eq!(*table.next_id.read(), u128::from(u64::MAX) + 1);
        assert_eq!(*table.maps_integer_keys.read(), expected);
        assert_eq!(
            catalog.get_metadata(&key).unwrap().as_deref(),
            Some(reservation)
        );
        drop((table, restored));
        assert_eq!(control.memory().used(), 0);
    }
    catalog
        .delete_metadata(identifiers::key_mapping::VERIFIED_KEY)
        .unwrap();
    let restored = finish(&mut input(&engine, &control)).unwrap();
    assert!(!*restored.read()[&relation].maps_integer_keys.read());
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}
