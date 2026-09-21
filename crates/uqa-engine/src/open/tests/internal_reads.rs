//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::Ordering;

fn persistent_engine(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[test]
fn internal_read_sessions_and_fixed_snapshots_do_not_register_maintenance_clients() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("internal-reads.db");
        let root = persistent_engine(provider, &path);
        root.sql("CREATE TABLE t(v integer); INSERT INTO t VALUES(1)", &[])
            .unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let internal = root.new_internal_read_session().unwrap();
        assert!(internal.session.statistics_worker.load(Ordering::Acquire));
        assert!(!internal.session.statistics_client.load(Ordering::Acquire));
        assert_eq!(
            internal.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
            Value::Int(1)
        );
        assert!(!internal.session.statistics_client.load(Ordering::Acquire));
        let pinned = root.open_independent_pinned_read_snapshot().unwrap();
        assert!(pinned.session.statistics_worker.load(Ordering::Acquire));
        assert!(!pinned.session.statistics_client.load(Ordering::Acquire));
        drop(pinned);
        let retained = root.open_retained_pinned_read_snapshot().unwrap();
        assert!(retained.session.statistics_worker.load(Ordering::Acquire));
        assert!(!retained.session.statistics_client.load(Ordering::Acquire));
        drop(retained);
        drop(internal);
        let public = root.new_session().unwrap();
        assert!(!public.session.statistics_worker.load(Ordering::Acquire));
        assert!(public.session.statistics_client.load(Ordering::Acquire));
        assert_eq!(
            public.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
            Value::Int(1)
        );
    }
}

#[test]
fn fixed_read_attachment_keeps_the_source_snapshot_while_latest_readers_advance() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let root = persistent_engine(provider, &directory.path().join("fixed-attachment.db"));
        root.sql("CREATE TABLE t(v integer); INSERT INTO t VALUES(1)", &[])
            .unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let peer = root.new_session().unwrap();
        peer.release_automatic_statistics_client();
        peer.session
            .statistics_worker
            .store(true, Ordering::Release);
        let backend = root.storage.backend.as_ref().unwrap().clone();
        backend.begin_read_transaction().unwrap();
        let original_version = backend.change_version().unwrap();
        let id = root.live_table_doc_ids("t").unwrap()[0];
        peer.sql("UPDATE t SET v = 2; CREATE TABLE later(v integer); CREATE SCHEMA later_namespace; CREATE SEQUENCE later_sequence; CREATE VIEW later_view AS SELECT 1 AS n; CREATE SERVER later_server FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE later_foreign (id INTEGER) SERVER later_server; CREATE INDEX later_index ON t(v); ALTER TABLE t ALTER COLUMN v SET DEFAULT 2; ALTER TABLE t ADD CONSTRAINT positive CHECK (v > 0)", &[])
            .unwrap();
        peer.register_named_analyzer("later_analyzer", r#"{"tokenizer":{"type":"whitespace"}}"#)
            .unwrap();
        peer.save_model(
            "later_model",
            &uqa_ml::DeepModel {
                layers: vec![uqa_ml::DeepLayerSpec::Dense {
                    weights: vec![1.0],
                    bias: vec![0.0],
                    input_channels: 1,
                    output_channels: 1,
                }],
                alpha: 0.0,
                gating: uqa_ml::GatingSpec::None,
            },
        )
        .unwrap();
        let retained = root.open_retained_pinned_read_snapshot().unwrap();
        let latest = root.open_independent_pinned_read_snapshot().unwrap();
        assert_eq!(
            retained.get_document("t", id).unwrap().unwrap()["v"],
            Value::Int(1)
        );
        assert_eq!(
            latest.get_document("t", id).unwrap().unwrap()["v"],
            Value::Int(2)
        );
        assert!(retained.try_table("later").unwrap().is_none());
        assert!(latest.try_table("later").unwrap().is_some());
        assert!(!retained.has_table("later").unwrap());
        assert!(latest.has_table("later").unwrap());
        assert!(retained.describe_table("later").unwrap().is_none());
        assert_eq!(latest.table_columns("later").unwrap(), ["v"]);
        assert_eq!(retained.table_names().unwrap(), ["public.t"]);
        assert_eq!(latest.table_names().unwrap(), ["public.later", "public.t"]);
        assert_attached_catalog_visibility(&retained, false);
        assert_attached_catalog_visibility(&latest, true);
        assert_eq!(
            retained
                .storage
                .backend
                .as_ref()
                .unwrap()
                .change_version()
                .unwrap(),
            original_version
        );
        backend
            .refresh_transaction_snapshot(&uqa_core::CancellationToken::new())
            .unwrap();
        assert_ne!(backend.change_version().unwrap(), original_version);
        backend.rollback_transaction().unwrap();
        drop((root, backend, latest));
        peer.sql("UPDATE t SET v = 3", &[]).unwrap();
        assert_eq!(
            retained.get_document("t", id).unwrap().unwrap()["v"],
            Value::Int(1)
        );
        assert!(retained.try_table("later").unwrap().is_none());
        assert!(!retained.has_table("later").unwrap());
        assert_eq!(retained.table_columns("t").unwrap(), ["v"]);
        assert_attached_catalog_visibility(&retained, false);
    }
}

fn assert_attached_namespace_visibility(engine: &Engine, visible: bool) {
    assert_eq!(engine.has_schema("later_namespace").unwrap(), visible);
    assert_eq!(engine.has_namespace("later_namespace").unwrap(), visible);
    assert_eq!(
        engine
            .list_schemas()
            .unwrap()
            .contains(&"later_namespace".into()),
        visible
    );
    engine.set_search_path(vec!["later_namespace".into(), "public".into()]);
    assert_eq!(
        engine.current_schema_name().unwrap().as_deref(),
        Some(if visible { "later_namespace" } else { "public" })
    );
    assert_eq!(
        engine
            .current_schema_names(false)
            .unwrap()
            .contains(&"later_namespace".into()),
        visible
    );
    assert_eq!(
        engine
            .tables_in_schema("public")
            .unwrap()
            .contains(&"later".into()),
        visible
    );
}

fn assert_attached_catalog_visibility(engine: &Engine, visible: bool) {
    assert_attached_namespace_visibility(engine, visible);
    assert_eq!(
        engine
            .list_sequences()
            .unwrap()
            .contains(&"public.later_sequence".into()),
        visible
    );
    assert_eq!(
        engine
            .sequences_snapshot()
            .unwrap()
            .contains_key("public.later_sequence"),
        visible
    );
    assert_eq!(
        engine.sequence_state("later_sequence").unwrap().is_some(),
        visible
    );
    assert_eq!(engine.view("later_view").unwrap().is_some(), visible);
    assert_eq!(
        engine
            .list_views()
            .unwrap()
            .contains(&"public.later_view".into()),
        visible
    );
    assert_eq!(
        engine
            .list_named_analyzers()
            .unwrap()
            .contains(&"later_analyzer".into()),
        visible
    );
    assert_eq!(
        engine.foreign_server("later_server").unwrap().is_some(),
        visible
    );
    assert_eq!(
        engine.foreign_table("later_foreign").unwrap().is_some(),
        visible
    );
    assert_eq!(
        engine
            .list_foreign_servers()
            .unwrap()
            .contains(&"later_server".into()),
        visible
    );
    assert_eq!(
        engine
            .list_foreign_tables()
            .unwrap()
            .contains(&"public.later_foreign".into()),
        visible
    );
    assert_eq!(
        engine.catalog_index("later_index").unwrap().is_some(),
        visible
    );
    assert_eq!(engine.has_catalog_index("later_index").unwrap(), visible);
    assert_eq!(
        engine
            .list_catalog_indexes()
            .unwrap()
            .iter()
            .any(|index| index.relation.name == "later_index"),
        visible
    );
    assert_eq!(
        engine.column_default_expr("t", "v").unwrap().is_some(),
        visible
    );
    assert_eq!(
        !engine
            .try_check_constraint_definitions("t")
            .unwrap()
            .is_empty(),
        visible
    );
    assert_eq!(engine.load_model("later_model").unwrap().is_some(), visible);
    assert_eq!(engine.transaction_depth(), 0);
}

#[test]
fn internal_read_workers_progress_while_the_source_statement_owns_its_transaction() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let root = persistent_engine(provider, &directory.path().join("worker-reads.db"));
        root.sql("CREATE TABLE t(v integer); INSERT INTO t VALUES(1)", &[])
            .unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let peer = root.new_session().unwrap();
        peer.release_automatic_statistics_client();
        peer.session
            .statistics_worker
            .store(true, Ordering::Release);
        let backend = root.storage.backend.as_ref().unwrap();
        backend.begin_read_transaction().unwrap();
        peer.sql("UPDATE t SET v = 2; CREATE TABLE later(v integer)", &[])
            .unwrap();

        for retained in [false, true] {
            std::thread::scope(|scope| {
                let statement = root.runtime.statement_gate.lock();
                let transactions = root.session.transactions.lock();
                let (sender, receiver) = mpsc::channel();
                let source = &root;
                let worker = scope.spawn(move || {
                    let reader = if retained {
                        source.new_internal_retained_read_session()
                    } else {
                        source.new_internal_read_session()
                    }
                    .unwrap();
                    let value = reader.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"].clone();
                    let has_later = reader.try_table("later").unwrap().is_some();
                    assert!(!reader.session.statistics_client.load(Ordering::Acquire));
                    sender.send((value, has_later)).unwrap();
                });
                let completed = receiver.recv_timeout(Duration::from_secs(10));
                // Release the parent locks before joining even on failure, so a regression reports an error instead of hanging the test executable.
                drop(transactions);
                drop(statement);
                worker.join().unwrap();
                let (value, has_later) =
                    completed.expect("internal reader waited for its parent statement");
                assert_eq!(value, Value::Int(if retained { 1 } else { 2 }));
                assert_eq!(has_later, !retained);
            });
        }
        backend.rollback_transaction().unwrap();
    }
}

#[test]
fn internal_document_adapters_keep_query_and_mutation_views_without_reentering_the_statement() {
    use uqa_execution::mutation::constraints::context::MutationRead;
    use uqa_execution::operator_tree::driver::context::RetrievalRelations;
    use uqa_execution::query::block::context::QueryDocumentRead;
    use uqa_execution::query::retrieval::context::RetrievalDocuments;
    use uqa_execution::serializable::SerializableWrites;

    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let root = persistent_engine(provider, &directory.path().join("document-adapters.db"));
        root.sql("CREATE TABLE t(v integer); INSERT INTO t VALUES(1)", &[])
            .unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let peer = root.new_session().unwrap();
        peer.release_automatic_statistics_client();
        peer.session
            .statistics_worker
            .store(true, Ordering::Release);
        let id = root.table_doc_ids("t").unwrap()[0];
        root.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM t", &[])
            .unwrap();
        let participant = root
            .serializable_session()
            .unwrap()
            .serializable_read_context()
            .unwrap()
            .unwrap()
            .id();
        peer.sql("UPDATE t SET v = 2; INSERT INTO t VALUES(3)", &[])
            .unwrap();
        root.sql("SHOW transaction_isolation", &[]).unwrap();

        std::thread::scope(|scope| {
            let statement = root.runtime.statement_gate.lock();
            let (sender, receiver) = mpsc::channel();
            let source = &root;
            let worker = scope.spawn(move || {
                let query = <Engine as RetrievalDocuments>::get_document(source, "t", id)
                    .unwrap()
                    .unwrap()["v"]
                    .clone();
                let mutation = <Engine as MutationRead>::get_document(source, "t", id)
                    .unwrap()
                    .unwrap()["v"]
                    .clone();
                let query_ids = <Engine as QueryDocumentRead>::document_ids(source, "t").unwrap();
                let retrieval_ids =
                    <Engine as RetrievalRelations>::table_doc_ids(source, "t").unwrap();
                sender
                    .send((query, mutation, query_ids, retrieval_ids))
                    .unwrap();
            });
            let completed = receiver.recv_timeout(Duration::from_secs(10));
            // Release the parent gate before joining so accidental reentry fails without hanging the executable.
            drop(statement);
            worker.join().unwrap();
            let (query, mutation, query_ids, retrieval_ids) =
                completed.expect("internal document read reentered its parent statement");
            assert_eq!(query, Value::Int(1));
            assert_eq!(mutation, Value::Int(2));
            assert_eq!(query_ids, vec![id]);
            assert_eq!(retrieval_ids, vec![id]);
        });
        assert_eq!(
            root.serializable_session()
                .unwrap()
                .serializable_read_context()
                .unwrap()
                .unwrap()
                .id(),
            participant
        );
        root.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn catalog_workers_keep_the_parent_snapshot_without_reentering_its_statement() {
    use uqa_execution::catalog::services::CatalogNamespace;
    use uqa_execution::mutation::constraints::context::ConstraintCatalog;
    use uqa_execution::operator_tree::driver::context::{RetrievalIndexes, RetrievalModels};
    use uqa_execution::query::graph_lifecycle::GraphLifecycle;
    use uqa_execution::query::table_functions::context::AnalyzerTableFunctions;
    use uqa_execution::serializable::SerializableWrites;
    use uqa_planner::statement_planning::PlannerStatisticsCatalog;
    use uqa_sql::expr::EngineHook;

    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let root = persistent_engine(provider, &directory.path().join("catalog-workers.db"));
        root.sql("CREATE TABLE t (v INTEGER PRIMARY KEY CHECK (v > 0)); CREATE INDEX t_v ON t(v); INSERT INTO t VALUES (1)", &[]).unwrap();
        root.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT v FROM t", &[])
            .unwrap();
        let original = root
            .serializable_session()
            .unwrap()
            .serializable_read_context()
            .unwrap()
            .unwrap()
            .id();
        std::thread::scope(|scope| {
            let statement = root.runtime.statement_gate.lock();
            let (sender, receiver) = mpsc::channel();
            let source = &root;
            let worker = scope.spawn(move || {
                assert_eq!(
                    EngineHook::current_schema(source).unwrap().as_deref(),
                    Some("public")
                );
                assert!(CatalogNamespace::current_schema_names(source, true)
                    .unwrap()
                    .contains(&"public".into()));
                assert!(GraphLifecycle::has_namespace(source, "public").unwrap());
                assert!(!PlannerStatisticsCatalog::catalog_indexes(source)
                    .unwrap()
                    .is_empty());
                assert!(RetrievalIndexes::catalog_index(source, "t_v")
                    .unwrap()
                    .is_some());
                assert!(!ConstraintCatalog::try_unique_columns(source, "t")
                    .unwrap()
                    .is_empty());
                assert!(
                    !ConstraintCatalog::try_check_constraint_definitions(source, "t")
                        .unwrap()
                        .is_empty()
                );
                assert!(ConstraintCatalog::try_foreign_keys(source, "t")
                    .unwrap()
                    .is_empty());
                assert!(AnalyzerTableFunctions::list_named_analyzers(source)
                    .unwrap()
                    .is_empty());
                assert!(RetrievalModels::load_model(source, "missing_model")
                    .unwrap()
                    .is_none());
                sender.send(()).unwrap();
            });
            let completed = receiver.recv_timeout(Duration::from_secs(10));
            // Release the gate before joining so a reentry regression reports a failure instead of hanging the executable.
            drop(statement);
            worker.join().unwrap();
            completed.expect("catalog worker reentered its parent statement");
        });
        assert_eq!(
            root.serializable_session()
                .unwrap()
                .serializable_read_context()
                .unwrap()
                .unwrap()
                .id(),
            original
        );
        assert_eq!(root.transaction_depth(), 1);
        root.sql("COMMIT", &[]).unwrap();
    }
}

#[test]
fn statistics_workers_keep_the_parent_statement_and_memory_lazy_collection() {
    use uqa_execution::query::table_functions::context::AnalyzerTableFunctions;
    use uqa_planner::statement_planning::PlannerStatisticsCatalog;

    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let root = if provider == 3 {
            Engine::new()
        } else {
            persistent_engine(provider, &directory.path().join("statistics-workers.db"))
        };
        root.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX t_body ON t USING gin(body); INSERT INTO t VALUES (1, 'token')", &[]).unwrap();
        root.run_analyze(Some("t")).unwrap();
        root.sql("BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT id FROM t", &[])
            .unwrap();
        if provider == 3 {
            root.require_table("t")
                .unwrap()
                .column_stats_dirty
                .store(true, Ordering::Release);
        }
        std::thread::scope(|scope| {
            let statement = root.runtime.statement_gate.lock();
            let (sender, receiver) = mpsc::channel();
            let source = &root;
            let worker = scope.spawn(move || {
                let stats = AnalyzerTableFunctions::fts_index_stats(source, Some("t")).unwrap();
                assert_eq!(stats.len(), 1);
                assert_eq!(stats[0].total_field_length, 1);
                assert!(PlannerStatisticsCatalog::column_statistics(source, "t")
                    .unwrap()
                    .contains_key("id"));
                sender.send(()).unwrap();
            });
            let completed = receiver.recv_timeout(Duration::from_secs(10));
            drop(statement);
            worker.join().unwrap();
            completed.expect("statistics worker reentered its parent statement");
        });
        assert_eq!(root.transaction_depth(), 1);
        root.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn attached_text_statistics_keep_their_original_index_data_and_table_inventory() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let root = persistent_engine(provider, &directory.path().join("statistics-readers.db"));
        root.sql("CREATE TABLE t (body TEXT); CREATE INDEX t_body ON t USING gin(body); INSERT INTO t VALUES ('token')", &[]).unwrap();
        root.release_automatic_statistics_client();
        root.session
            .statistics_worker
            .store(true, Ordering::Release);
        let peer = root.new_session().unwrap();
        peer.release_automatic_statistics_client();
        peer.session
            .statistics_worker
            .store(true, Ordering::Release);
        let backend = root.storage.backend.as_ref().unwrap().clone();
        backend.begin_read_transaction().unwrap();
        assert_eq!(
            root.fts_index_stats(Some("t")).unwrap()[0].total_field_length,
            1
        );
        peer.sql("UPDATE t SET body = 'token token token'; CREATE TABLE later (body TEXT); CREATE INDEX later_body ON later USING gin(body); INSERT INTO later VALUES ('later')", &[]).unwrap();
        let retained = root.open_retained_pinned_read_snapshot().unwrap();
        let latest = root.open_independent_pinned_read_snapshot().unwrap();
        assert_eq!(retained.fts_index_stats(None).unwrap().len(), 1);
        assert_eq!(latest.fts_index_stats(None).unwrap().len(), 2);
        assert_eq!(
            retained.fts_index_stats(Some("t")).unwrap()[0].total_field_length,
            1
        );
        assert_eq!(
            latest.fts_index_stats(Some("t")).unwrap()[0].total_field_length,
            3
        );
        backend.rollback_transaction().unwrap();
        drop((root, backend, latest));
        peer.sql("UPDATE t SET body = 'token token token token'", &[])
            .unwrap();
        assert_eq!(retained.fts_index_stats(None).unwrap().len(), 1);
        assert_eq!(
            retained.fts_index_stats(Some("t")).unwrap()[0].total_field_length,
            1
        );
    }
}

#[test]
fn mutation_exact_probes_use_current_documents_and_indexes_without_reentering_the_statement() {
    use uqa_execution::mutation::constraints::context::MutationIndexRead;
    use uqa_execution::mutation::point_update::context::PointMutationStorage;
    use uqa_storage::ValueIndexKey;

    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let root = persistent_engine(provider, &directory.path().join("exact-workers.db"));
        root.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, k TEXT UNIQUE, v INTEGER); INSERT INTO t VALUES (1, 'old', 1)", &[]).unwrap();
        let peer = root.new_session().unwrap();
        let id = root.table_doc_ids("t").unwrap()[0];
        root.sql(
            "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT v FROM t",
            &[],
        )
        .unwrap();
        peer.sql("UPDATE t SET k = 'new', v = 2", &[]).unwrap();
        root.sql("SHOW transaction_isolation", &[]).unwrap();
        std::thread::scope(|scope| {
            let statement = root.runtime.statement_gate.lock();
            let (sender, receiver) = mpsc::channel();
            let source = &root;
            let worker = scope.spawn(move || {
                assert_eq!(
                    PointMutationStorage::find_doc_id_by_field(source, "t", "v", &Value::Int(2))
                        .unwrap(),
                    Some(id)
                );
                assert_eq!(
                    MutationIndexRead::find_conflict(
                        source,
                        "t",
                        &["k".into()],
                        &[Value::Str("new".into())]
                    )
                    .unwrap(),
                    Some(id)
                );
                assert_eq!(
                    MutationIndexRead::find_conflict(
                        source,
                        "t",
                        &["v".into(), "k".into()],
                        &[Value::Int(2), Value::Str("new".into())]
                    )
                    .unwrap(),
                    Some(id)
                );
                assert_eq!(
                    MutationIndexRead::value_index_scan_key(
                        source,
                        "t",
                        &ValueIndexKey::Column("k".into()),
                        &uqa_core::Predicate::Equals(Value::Str("new".into()))
                    )
                    .unwrap()
                    .unwrap()
                    .len(),
                    1
                );
                sender.send(()).unwrap();
            });
            let completed = receiver.recv_timeout(Duration::from_secs(10));
            drop(statement);
            worker.join().unwrap();
            completed.expect("mutation lookup reentered its parent statement");
        });
        assert_eq!(
            root.find_conflict("t", &["k".into()], &[Value::Str("old".into())])
                .unwrap(),
            Some(id)
        );
        assert_eq!(
            root.find_conflict("t", &["k".into()], &[Value::Str("new".into())])
                .unwrap(),
            None
        );
        assert_eq!(
            root.find_doc_id_by_field("t", "v", &Value::Int(1)).unwrap(),
            Some(id)
        );
        root.sql("ROLLBACK", &[]).unwrap();
        drop(peer);
        drop(root);
        let reopened = persistent_engine(provider, &directory.path().join("exact-workers.db"));
        assert_eq!(
            reopened
                .find_conflict("t", &["k".into()], &[Value::Str("new".into())])
                .unwrap(),
            Some(id)
        );
    }
}
