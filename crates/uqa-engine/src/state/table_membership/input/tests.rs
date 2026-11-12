//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{tests::providers::Provider, Engine};
use uqa_core::Value;

fn name(value: &str, control: &StorageReadControl) -> Budgeted<RelationIdentity> {
    copy_name(
        &RelationIdentity::new("public", value),
        &ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        ),
    )
    .unwrap()
}

fn take_table(engine: &Engine, name: &str) -> Arc<TableState> {
    let relation = engine
        .storage
        .tables
        .read()
        .keys()
        .find(|relation| relation.name == name)
        .unwrap()
        .clone();
    engine
        .storage
        .tables
        .write()
        .remove(&relation)
        .unwrap()
        .unwrap()
}

#[test]
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
fn admitted_registry_captures_original_membership_after_construction_is_cancelled() {
    let mut engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE kept(id integer); INSERT INTO kept VALUES(1)",
            &[],
        )
        .unwrap();
    let table = take_table(&engine, "kept");
    let control = StorageReadControl::with_limit(4 << 20);
    engine.storage.tables = Arc::new(
        TableRegistry::from_native_tables(
            [(name("kept", &control), table)],
            engine.storage.memory_capture.as_ref(),
            &control,
        )
        .unwrap(),
    );
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    let output = StorageReadControl::with_limit(8 << 20);
    let cut = engine.capture_committed_memory(&output).unwrap();
    engine
        .sql(
            "ALTER TABLE kept RENAME TO moved; INSERT INTO moved VALUES(2)",
            &[],
        )
        .unwrap();
    drop(engine);
    drop(occupied);
    let table = cut.table(&RelationIdentity::new("public", "kept")).unwrap();
    assert_eq!(table.document_store.len().unwrap(), 1);
    assert_eq!(
        table.document_store.get_field(1, "id").unwrap(),
        Some(Value::Int(1))
    );
    assert!(control.memory().used() > 0);
    drop((table, cut));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn admitted_registry_preserves_lookup_names_and_remains_writable_after_input_retirement(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut engine = provider.open(&directory.path().join("registry.db"));
    engine.sql("CREATE TABLE kept(id integer PRIMARY KEY, body text); INSERT INTO kept VALUES(1,'original')", &[]).unwrap();
    let table = take_table(&engine, "kept");
    let object = table.object_id();
    let control = StorageReadControl::with_limit(4 << 20);
    let name = name("kept", &control);
    let pointer = name.name.as_ptr();
    let tables = TableRegistry::from_native_tables(
        [(name, table)],
        engine.storage.memory_capture.as_ref(),
        &control,
    )
    .unwrap();
    let original = tables.live_snapshot();
    assert_eq!(original.keys().next().unwrap().name.as_ptr(), pointer);
    engine.storage.tables = Arc::new(tables);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    engine.sql("BEGIN; ALTER TABLE kept RENAME TO moved; INSERT INTO moved VALUES(2,'second'); SAVEPOINT preserved; DROP TABLE moved; ROLLBACK TO preserved; COMMIT", &[]).unwrap();
    let rows = engine
        .sql("SELECT id FROM moved ORDER BY id", &[])
        .unwrap()
        .rows;
    assert_eq!(
        rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
        [Value::Int(1), Value::Int(2)]
    );
    let old = RelationIdentity::new("public", "kept");
    assert_eq!(original.get(&old).unwrap().object_id(), object);
    assert_eq!(original.keys().next().unwrap().name.as_ptr(), pointer);
    assert_eq!(original.len(), 1);
    drop(engine);
    drop(occupied);
    assert!(control.memory().used() > 0);
    drop(original);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn admitted_registry_rejects_invalid_identity_or_control_and_retires_its_prefix() {
    for memory_source in [false, true] {
        for invalid in 0..7 {
            let engine = Engine::new();
            engine.sql("CREATE TABLE first(id integer); CREATE TABLE second(id integer); CREATE TEMP TABLE private_temp(id integer)", &[]).unwrap();
            let first = take_table(&engine, "first");
            let second = take_table(
                &engine,
                if invalid == 4 {
                    "private_temp"
                } else {
                    "second"
                },
            );
            let control = StorageReadControl::with_limit(1 << 20);
            let other = StorageReadControl::with_limit(1 << 20);
            let registry = memory_source.then(|| engine.storage.memory_capture.as_ref().unwrap());
            let input = TableRegistryInput::new(registry, &control)
                .unwrap()
                .add_table(name("first", &control), first)
                .unwrap();
            let key = match invalid {
                0 => name("first", &control),
                1 => name("second", &other),
                2 => Budgeted::new(
                    RelationIdentity::new("public", "second"),
                    control.memory().empty_reservation(),
                ),
                3 => name("", &control),
                _ => name("second", &control),
            };
            let occupied = (invalid == 6).then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            if invalid == 5 {
                control.cancellation().cancel();
            }
            let result = input.add_table(key, second);
            assert!(matches!(result, Err(StorageBackendError::Cancelled(_))) == (invalid == 5));
            assert!(result.is_err());
            assert_eq!(
                control.memory().used(),
                occupied
                    .as_ref()
                    .map_or(0, uqa_core::memory::MemoryReservation::bytes)
            );
            assert_eq!(other.memory().used(), 0);
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn admitted_registry_completion_failure_retires_native_and_registered_roots() {
    for memory_source in [false, true] {
        for cancelled in [false, true] {
            let engine = Engine::new();
            engine.sql("CREATE TABLE kept(id integer)", &[]).unwrap();
            let table = take_table(&engine, "kept");
            let control = StorageReadControl::with_limit(1 << 20);
            let registry = memory_source.then(|| engine.storage.memory_capture.as_ref().unwrap());
            let input = TableRegistryInput::new(registry, &control)
                .unwrap()
                .add_table(name("kept", &control), table)
                .unwrap();
            let occupied = control
                .memory()
                .reserve(control.memory().available())
                .unwrap();
            if cancelled {
                control.cancellation().cancel();
            }
            assert!(input.finish().is_err());
            assert_eq!(control.memory().used(), occupied.bytes());
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
        }
    }
}
