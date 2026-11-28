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

#[rstest::rstest]
#[case::memory(crate::tests::providers::Provider::Memory)]
#[case::sqlite_native(crate::tests::providers::Provider::SQLiteNative)]
#[case::sqlite_kv(crate::tests::providers::Provider::SQLiteKeyValue)]
#[case::redb(crate::tests::providers::Provider::Redb)]
fn catalog_generations_reuse_selected_tables_within_their_own_allowance(
    #[case] provider: crate::tests::providers::Provider,
) {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken, RelationIdentity,
    };
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("catalog-generations"));
    pause_automatic_statistics(&engine);
    engine
        .sql(
            "CREATE TABLE shared_catalog(id integer); CREATE TYPE selected_range AS RANGE (subtype=text)",
            &[],
        )
        .unwrap();
    let relation = RelationIdentity::new("public", "shared_catalog");
    engine.clear_regtype_output_cache();
    CATALOG_CAPTURES.set(0);
    let first = engine.catalog_read_view();
    let range = first
        .ranges()
        .find(|range| range.identity.name == "selected_range")
        .unwrap()
        .oid;
    let descriptor = first.range_value_descriptor(range).unwrap().unwrap();
    for _ in 0..8 {
        let hit = uqa_sql::expr::SQLValueCatalog::value_range_type(&engine, range)
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&descriptor, &hit));
        let current = engine.catalog_read_view();
        assert!(std::ptr::eq(first.snapshot(), current.snapshot()));
    }
    assert_eq!(CATALOG_CAPTURES.get(), 1);

    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let charged = engine.catalog_read_view_with_control(&control).unwrap();
    let additional = engine.catalog_read_view_with_control(&control).unwrap();
    assert!(std::ptr::eq(charged.snapshot(), additional.snapshot()));
    assert_eq!(CATALOG_CAPTURES.get(), 2);
    let tiny = MemoryBudget::new(1);
    let error = engine
        .catalog_read_view_with_control(&ProductionControl::new(&tiny, &token, &token))
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(tiny.used(), 0);
    token.cancel();
    let error = engine
        .catalog_read_view_with_control(&control)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(memory.used() > 0);
    drop((charged, additional));
    assert_eq!(memory.used(), 0);

    engine.sql("BEGIN; ALTER TABLE shared_catalog ADD COLUMN private_value text; SAVEPOINT before_second; ALTER TABLE shared_catalog ADD COLUMN second integer", &[]).unwrap();
    assert_eq!(
        engine.catalog_read_view().snapshot().tables[&relation]
            .columns
            .len(),
        3
    );
    engine.sql("ROLLBACK TO before_second", &[]).unwrap();
    assert_eq!(
        engine.catalog_read_view().snapshot().tables[&relation]
            .columns
            .len(),
        2
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        engine.catalog_read_view().snapshot().tables[&relation]
            .columns
            .len(),
        1
    );
    assert_eq!(first.snapshot().tables[&relation].columns.len(), 1);
}

#[rstest::rstest]
#[case::sqlite_native(crate::tests::providers::Provider::SQLiteNative)]
#[case::sqlite_kv(crate::tests::providers::Provider::SQLiteKeyValue)]
#[case::redb(crate::tests::providers::Provider::Redb)]
fn cached_catalog_observes_peer_commits_and_keeps_session_overlays_private(
    #[case] provider: crate::tests::providers::Provider,
) {
    use uqa_core::RelationIdentity;
    let directory = tempfile::tempdir().unwrap();
    let root = provider.open(&directory.path().join("shared"));
    pause_automatic_statistics(&root);
    root.sql("CREATE TABLE shared_rows(id integer)", &[])
        .unwrap();
    root.create_graph("committed_graph").unwrap();
    let peer = root.new_session().unwrap();
    pause_automatic_statistics(&peer);
    let relation = RelationIdentity::new("public", "shared_rows");
    let previous = peer.catalog_read_view();
    assert_eq!(previous.snapshot().tables[&relation].columns.len(), 1);
    root.sql("ALTER TABLE shared_rows ADD COLUMN added integer", &[])
        .unwrap();
    peer.sql("SELECT added FROM shared_rows", &[]).unwrap();
    assert_eq!(
        peer.catalog_read_view().snapshot().tables[&relation]
            .columns
            .len(),
        2
    );
    assert_eq!(previous.snapshot().tables[&relation].columns.len(), 1);

    peer.sql("CREATE TEMP TABLE local_only(id integer)", &[])
        .unwrap();
    assert!(peer
        .catalog_read_view()
        .snapshot()
        .temporary_namespace
        .is_some());
    assert!(root
        .catalog_read_view()
        .snapshot()
        .temporary_namespace
        .is_none());
    peer.sql(
        "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT id FROM shared_rows",
        &[],
    )
    .unwrap();
    peer.create_graph("private_graph").unwrap();
    let graph = peer.catalog_read_view();
    assert!(graph
        .graph_names()
        .iter()
        .any(|name| name == "private_graph"));
    assert!(!root
        .catalog_read_view()
        .graph_names()
        .iter()
        .any(|name| name == "private_graph"));
    peer.sql("ROLLBACK", &[]).unwrap();
    let current = peer.catalog_read_view();
    assert!(!current
        .graph_names()
        .iter()
        .any(|name| name == "private_graph"));
    assert!(graph
        .graph_names()
        .iter()
        .any(|name| name == "private_graph"));
}

#[rstest::rstest]
#[case::sqlite_native(crate::tests::providers::Provider::SQLiteNative)]
#[case::sqlite_kv(crate::tests::providers::Provider::SQLiteKeyValue)]
#[case::redb(crate::tests::providers::Provider::Redb)]
fn reopened_catalog_retains_constraint_indexes(
    #[case] provider: crate::tests::providers::Provider,
) {
    use uqa_core::memory::{MemoryBudget, ProductionControl, RetainedValue};
    use uqa_core::{CancellationToken, RelationIdentity};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constraints.db");
    let index = RelationIdentity::new("public", "docs_pkey");
    {
        let engine = provider.open(&path);
        pause_automatic_statistics(&engine);
        engine
            .sql("CREATE TABLE docs(id integer PRIMARY KEY, body text)", &[])
            .unwrap();
    }
    let engine = provider.open(&path);
    pause_automatic_statistics(&engine);
    let loaded = engine.durable.catalog_indexes.snapshot();
    assert!(loaded.contains_key(&index));
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let ordinary = engine.restored_catalog_read_view();
    let charged = engine.catalog_read_view_with_control(&control).unwrap();
    for view in [&ordinary, &charged] {
        assert!(
            RetainedValue::ptr_eq(&loaded, &view.snapshot().definitions.catalog_indexes),
            "catalog capture must select the restored index definitions"
        );
    }
    engine
        .sql("ALTER TABLE docs DROP CONSTRAINT docs_pkey", &[])
        .unwrap();
    assert!(!engine
        .catalog_read_view()
        .snapshot()
        .definitions
        .catalog_indexes
        .contains_key(&index));
    assert!(ordinary
        .snapshot()
        .definitions
        .catalog_indexes
        .contains_key(&index));
    engine
        .sql("ALTER TABLE docs ADD PRIMARY KEY (id)", &[])
        .unwrap();
    drop((ordinary, charged, loaded, engine));
    assert_eq!(budget.used(), 0);
    let engine = provider.open(&path);
    engine.sql("DROP TABLE docs", &[]).unwrap();
}
