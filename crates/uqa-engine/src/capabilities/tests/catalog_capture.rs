//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog work comparisons hold maintenance history constant and separately exercise intervening commits.

use super::{Engine, SQLParam, Value, CATALOG_CAPTURES};
use std::sync::{atomic::Ordering, Arc};

pub(super) fn pause_automatic_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn catalog_capture_work_stays_bounded_after_intervening_statistics(#[case] provider: usize) {
    let captures = [1, 32].map(|tables| {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.db");
        let engine = match provider {
            0 => Engine::open(&path).unwrap(),
            1 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            2 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => unreachable!(),
        };
        pause_automatic_statistics(&engine);
        for index in 0..tables {
            engine
                .sql(
                    &format!("CREATE TABLE item_{index} (id integer, title text, active boolean)"),
                    &[],
                )
                .unwrap();
        }
        let ordinary = capture_columns(&engine);
        let relation = uqa_core::RelationIdentity::new("public", "item_0");
        let previous_table = Arc::clone(&engine.storage.tables.read()[&relation]);
        interleave_statistics_with_catalog_adoption(&engine);
        let refreshed = capture_columns(&engine);
        let refreshed_table = Arc::clone(&engine.storage.tables.read()[&relation]);
        assert!(!Arc::ptr_eq(&previous_table, &refreshed_table));
        assert_eq!(engine.column_stats("item_0").unwrap().len(), 3);
        let unchanged = capture_columns(&engine);
        assert!(Arc::ptr_eq(
            &refreshed_table,
            &engine.storage.tables.read()[&relation]
        ));
        assert!(ordinary > 0 && refreshed > ordinary);
        assert!(unchanged <= ordinary);
        [ordinary, refreshed, unchanged]
    });
    for (small, large) in captures[0].into_iter().zip(captures[1]) {
        assert!(
            large <= small,
            "catalog captures grew under the same commit history: {captures:?}"
        );
    }
}

fn interleave_statistics_with_catalog_adoption(engine: &Engine) {
    let peer = engine.new_session().unwrap();
    pause_automatic_statistics(&peer);
    let before_view = engine.epochs.seen_storage_read_view.lock().clone();
    let before_revisions = engine.epochs.storage_cache_revisions.lock().clone();
    engine
        .sql("CREATE TABLE pending_capture (id integer)", &[])
        .unwrap();
    assert!(peer.run_automatic_analyze("public.item_0").unwrap());
    // Replay adoption against the pre-commit observation, with the peer's commit already durable. This is the same boundary an asynchronous maintenance commit can cross, without depending on a worker deadline or thread schedule.
    *engine.epochs.seen_storage_read_view.lock() = before_view.clone();
    *engine.epochs.storage_cache_revisions.lock() = before_revisions.clone();
    engine.adopt_own_catalog_commit_revisions();
    assert!(engine.epochs.seen_storage_read_view.lock().as_ref() == before_view.as_ref());
    assert_eq!(
        *engine.epochs.storage_cache_revisions.lock(),
        before_revisions,
        "an intervening commit requires the ordinary refresh"
    );
}

fn capture_columns(engine: &Engine) -> usize {
    CATALOG_CAPTURES.set(0);
    let result = engine
        .sql(
            "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = $1 ORDER BY ordinal_position",
            &[SQLParam::scalar(Value::Str("item_0".into()))],
        )
        .unwrap();
    let captures = CATALOG_CAPTURES.get();
    assert_eq!(result.rows.len(), 3);
    for (row, (name, ty)) in
        result
            .rows
            .iter()
            .zip([("id", "integer"), ("title", "text"), ("active", "boolean")])
    {
        assert_eq!(row["column_name"], Value::Str(name.into()));
        assert_eq!(row["data_type"], Value::Str(ty.into()));
    }
    captures
}

#[rstest::rstest]
#[case::memory(crate::tests::providers::Provider::Memory)]
#[case::sqlite_native(crate::tests::providers::Provider::SQLiteNative)]
#[case::sqlite_kv(crate::tests::providers::Provider::SQLiteKeyValue)]
#[case::redb(crate::tests::providers::Provider::Redb)]
fn retained_engine_catalog_capture_keeps_completed_generations_after_ddl_and_engine_retirement(
    #[case] provider: crate::tests::providers::Provider,
) {
    use uqa_core::{
        image_io::{ImageDecodeWindow, ImageDecoder},
        memory::{MemoryBudget, RetainedValue},
        CancellationToken,
    };
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("captured-catalog"));
    pause_automatic_statistics(&engine);
    engine.sql("CREATE TABLE items(id integer); CREATE TABLE later_item(body text); CREATE VIEW capture_view AS SELECT id FROM items; CREATE DOMAIN captured_type AS integer; CREATE TEMP TABLE temporary_item(id integer)", &[]).unwrap();
    let expected = engine.restored_catalog_read_view();
    let durable = engine.durable.snapshot();
    let tables = engine.storage.tables.live_snapshot();
    let mut selected = None;
    let allocation = allocation_counter::measure(|| {
        selected = Some(engine.catalog_read_capture(&durable, None));
    });
    assert_eq!(allocation.bytes_total, 0);
    let capture = selected.unwrap();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let original = token.clone();
    let retained_native = native.clone();
    let mut input =
        ImageDecoder::<_, uqa_sql::SQLError>::new(&work, &token, |mut reader| async move {
            let view = capture
                .capture_from_input(
                    tables.iter().map(crate::TableState::catalog_snapshot_entry),
                    &retained_native,
                    &original,
                    &mut reader.input(),
                )
                .await?;
            Ok((engine, view))
        })
        .unwrap();
    assert!(input
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .unwrap()
        .is_none());
    let (engine, actual) = input.read(&mut std::io::empty()).unwrap();
    engine.sql("ALTER TABLE later_item ADD COLUMN added integer; DROP VIEW capture_view; DROP DOMAIN captured_type; CREATE TABLE after_capture(id integer)", &[]).unwrap();
    drop((durable, engine));
    assert_eq!(
        actual.snapshot().temporary_namespace,
        expected.snapshot().temporary_namespace
    );
    assert_eq!(
        actual.snapshot().tables.keys().collect::<Vec<_>>(),
        expected.snapshot().tables.keys().collect::<Vec<_>>()
    );
    for (name, table) in &actual.snapshot().tables {
        let original = &expected.snapshot().tables[name];
        assert!(table.columns.shares_definition(&original.columns));
        assert_eq!(table.object_id, original.object_id);
        assert_eq!(table.catalog_oids, original.catalog_oids);
    }
    macro_rules! shared { ($($field:ident),+ $(,)?) => { $(assert!(RetainedValue::ptr_eq(&actual.snapshot().definitions.$field, &expected.snapshot().definitions.$field), stringify!($field));)+ }; }
    shared!(
        builtin_routine_security,
        foreign_wrappers,
        foreign_servers,
        sql_user_functions,
        role_memberships,
        domains,
        enums,
        ranges,
        shells,
        comments,
        composites,
        graphs,
        views,
        catalog_indexes,
        database_security,
        schemas,
        sequences,
        graph_catalog_oids,
        system_relation_security,
        roles,
        triggers,
        rules
    );
    assert!(Arc::ptr_eq(
        &actual.snapshot().definitions.foreign_tables,
        &expected.snapshot().definitions.foreign_tables
    ));
    assert!(Arc::ptr_eq(
        &actual.snapshot().definitions.foreign_table_security,
        &expected.snapshot().definitions.foreign_table_security
    ));
    assert_eq!(work.used(), 0);
    assert!(native.used() > 0);
    drop((actual, expected));
    assert_eq!(native.used(), 0);
}
