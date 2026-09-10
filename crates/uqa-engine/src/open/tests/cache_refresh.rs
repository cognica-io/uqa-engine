//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::Engine;

#[test]
fn fixed_graph_snapshot_survives_unrelated_writer_promotion() {
    use uqa_graph::GraphStore as _;
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("graph-fixed-writer.db")).unwrap();
    pause_automatic_statistics(&root);
    root.sql("CREATE TABLE log_entries (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    root.create_graph("items").unwrap();
    root.add_graph_vertex(uqa_core::Vertex::new(1, "Item"), "items")
        .unwrap();
    let reader = root.new_session().unwrap();
    pause_automatic_statistics(&reader);
    let ids = |engine: &Engine| {
        engine
            .graph_with("items", |store| store.vertex_ids_in_graph("items"))
            .unwrap()
            .unwrap()
            .unwrap()
    };
    reader
        .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_eq!(ids(&reader), std::collections::BTreeSet::from([1]));
    root.add_graph_vertex(uqa_core::Vertex::new(2, "Item"), "items")
        .unwrap();
    reader
        .sql("INSERT INTO log_entries VALUES (1)", &[])
        .unwrap();
    assert_eq!(
        ids(&reader),
        std::collections::BTreeSet::from([1]),
        "writer promotion must not replace the transaction's graph read view"
    );
    reader.sql("COMMIT", &[]).unwrap();
    assert_eq!(ids(&reader), std::collections::BTreeSet::from([1, 2]));
}

#[test]
fn graph_cursor_keeps_declaration_state_after_own_graph_write() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("graph-cursor.db")).unwrap();
    pause_automatic_statistics(&engine);
    engine.create_graph("items").unwrap();
    engine
        .add_graph_vertex(uqa_core::Vertex::new(1, "Item"), "items")
        .unwrap();
    engine.sql("BEGIN; DECLARE item_cursor CURSOR FOR SELECT * FROM cypher('items', $$ MATCH (n) RETURN id(n) $$) AS result(id agtype)", &[]).unwrap();
    engine
        .add_graph_vertex(uqa_core::Vertex::new(2, "Item"), "items")
        .unwrap();
    let rows = engine.sql("FETCH ALL FROM item_cursor", &[]).unwrap().rows;
    assert_eq!(
        rows.len(),
        1,
        "the cursor must read the graph from DECLARE, not FETCH"
    );
    engine.sql("ROLLBACK", &[]).unwrap();
}

fn pause_automatic_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

#[test]
fn fixed_graph_overlay_preserves_own_writes_and_savepoints_on_both_sqlite_formats() {
    use std::collections::BTreeSet;
    use uqa_graph::GraphStore as _;
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph-overlay.db");
        let root = if compressed {
            Engine::open_compressed(
                &path,
                uqa_storage_sqlite::SQLiteCompressionOptions::default(),
            )
        } else {
            Engine::open(&path)
        }
        .unwrap();
        pause_automatic_statistics(&root);
        root.create_graph("items").unwrap();
        for id in [1, 2] {
            root.add_graph_vertex(uqa_core::Vertex::new(id, "Item"), "items")
                .unwrap();
        }
        let reader = root.new_session().unwrap();
        pause_automatic_statistics(&reader);
        let ids = |engine: &Engine| {
            engine
                .graph_with("items", |store| store.vertex_ids_in_graph("items"))
                .unwrap()
                .unwrap()
                .unwrap()
        };
        reader
            .sql(
                "BEGIN ISOLATION LEVEL REPEATABLE READ; SAVEPOINT before_first_read",
                &[],
            )
            .unwrap();
        assert_eq!(ids(&reader), BTreeSet::from([1, 2]));
        root.add_graph_vertex(uqa_core::Vertex::new(3, "Item"), "items")
            .unwrap();
        reader
            .add_graph_vertex(uqa_core::Vertex::new(4, "Item"), "items")
            .unwrap();
        assert_eq!(ids(&reader), BTreeSet::from([1, 2, 4]));
        reader.sql("SAVEPOINT own_changes", &[]).unwrap();
        reader
            .add_graph_vertex(uqa_core::Vertex::new(5, "Item"), "items")
            .unwrap();
        reader
            .graph_with_mut("items", |store| store.remove_vertex(2, "items"))
            .unwrap()
            .unwrap();
        assert_eq!(ids(&reader), BTreeSet::from([1, 4, 5]));
        reader.sql("ROLLBACK TO own_changes", &[]).unwrap();
        assert_eq!(ids(&reader), BTreeSet::from([1, 2, 4]));
        reader.sql("ROLLBACK TO before_first_read", &[]).unwrap();
        assert_eq!(ids(&reader), BTreeSet::from([1, 2]));
        reader
            .add_graph_vertex(uqa_core::Vertex::new(6, "Item"), "items")
            .unwrap();
        assert_eq!(ids(&reader), BTreeSet::from([1, 2, 6]));
        reader.sql("COMMIT", &[]).unwrap();
        assert_eq!(ids(&root), BTreeSet::from([1, 2, 3, 6]));
        assert!(reader.session.state.read().graph_overlay.is_none());
    }
}

#[test]
fn graph_snapshot_write_conflict_reports_serialization_failure_without_lost_update() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("graph-conflict.db")).unwrap();
    pause_automatic_statistics(&root);
    root.create_graph("items").unwrap();
    root.add_graph_vertex(uqa_core::Vertex::new(1, "Item"), "items")
        .unwrap();
    let reader = root.new_session().unwrap();
    pause_automatic_statistics(&reader);
    reader.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM cypher('items', $$ MATCH (n) RETURN n $$) AS result(n agtype)", &[]).unwrap();
    root.run_cypher(
        "items",
        "MATCH (n) SET n.value = 20 RETURN n",
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    let error = reader.sql("SELECT * FROM cypher('items', $$ MATCH (n) SET n.value = 30 RETURN n $$) AS result(n agtype)", &[]).unwrap_err();
    assert!(
        matches!(error, uqa_sql::SQLError::Routine { ref sqlstate, .. } if sqlstate == "40001"),
        "{error:?}"
    );
    reader.sql("ROLLBACK", &[]).unwrap();
    let (_, rows) = root
        .run_cypher(
            "items",
            "MATCH (n) RETURN n.value AS value",
            std::collections::BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(rows[0]["value"], uqa_core::Value::Int(20));
}

#[test]
fn graph_cursor_spools_only_dependencies_and_includes_own_prior_writes() {
    use uqa_graph::GraphStore as _;
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph-cursor-own.db");
        let engine = if compressed {
            Engine::open_compressed(
                &path,
                uqa_storage_sqlite::SQLiteCompressionOptions::default(),
            )
        } else {
            Engine::open(&path)
        }
        .unwrap();
        pause_automatic_statistics(&engine);
        engine.create_graph("items").unwrap();
        engine.create_graph("unrelated").unwrap();
        engine
            .add_graph_vertex(uqa_core::Vertex::new(1, "Item"), "items")
            .unwrap();
        engine
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .save_vertex(99, "Unused", "malformed unused payload")
            .unwrap();
        engine
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .save_graph_membership("vertex", 99, "unrelated")
            .unwrap();
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .add_graph_vertex(uqa_core::Vertex::new(2, "Item"), "items")
            .unwrap();
        engine.sql("DECLARE item_cursor SCROLL CURSOR WITH HOLD FOR SELECT * FROM cypher('items', $$ MATCH (n) RETURN id(n) $$) AS result(id agtype)", &[]).unwrap();
        engine
            .graph_with_mut("items", |store| store.remove_vertex(1, "items"))
            .unwrap()
            .unwrap();
        engine
            .add_graph_vertex(uqa_core::Vertex::new(3, "Item"), "items")
            .unwrap();
        let rows = engine.sql("FETCH ALL FROM item_cursor", &[]).unwrap().rows;
        assert_eq!(
            rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
            vec![
                uqa_core::Value::Str("1".into()),
                uqa_core::Value::Str("2".into())
            ]
        );
        engine.sql("COMMIT", &[]).unwrap();
        engine.sql("MOVE ABSOLUTE 0 FROM item_cursor", &[]).unwrap();
        assert_eq!(
            engine.sql("FETCH ALL FROM item_cursor", &[]).unwrap().rows,
            rows
        );
        engine.sql("CLOSE item_cursor", &[]).unwrap();
    }
}

#[test]
fn data_commits_retain_schema_and_decoded_statistics_across_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("data-refresh.db")).unwrap();
    pause_automatic_statistics(&writer);
    writer.sql("CREATE TABLE changed (id INTEGER PRIMARY KEY, body TEXT); CREATE TABLE untouched (id INTEGER PRIMARY KEY); INSERT INTO changed VALUES (1, 'first'); ANALYZE changed", &[]).unwrap();
    let reader = writer.new_session().unwrap();
    pause_automatic_statistics(&reader);
    reader.sql("SELECT * FROM changed", &[]).unwrap();
    let before = reader.require_table("changed").unwrap();
    let schema = before.columns.snapshot();
    let statistics = before.column_stats.snapshot();
    let unrelated = reader.require_table("untouched").unwrap();
    let registries = reader.durable.schemas.snapshot();
    for id in 2..12 {
        writer
            .sql(&format!("INSERT INTO changed VALUES ({id}, 'next')"), &[])
            .unwrap();
        assert_eq!(
            reader.sql("SELECT * FROM changed", &[]).unwrap().rows.len(),
            id as usize
        );
        let after = reader.require_table("changed").unwrap();
        assert!(
            Arc::ptr_eq(&before, &after),
            "data-only commit rebuilt the table catalog"
        );
        assert!(Arc::ptr_eq(&schema, &after.columns.snapshot()));
        assert!(
            Arc::ptr_eq(&statistics, &after.column_stats.snapshot()),
            "data-only commit decoded unchanged statistics"
        );
        assert!(Arc::ptr_eq(
            &unrelated,
            &reader.require_table("untouched").unwrap()
        ));
        assert!(Arc::ptr_eq(&registries, &reader.durable.schemas.snapshot()));
    }
}

#[test]
fn analyze_replaces_only_affected_statistics_and_shares_the_decoded_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("stats-refresh.db")).unwrap();
    pause_automatic_statistics(&writer);
    writer.sql("CREATE TABLE changed (id INTEGER PRIMARY KEY); CREATE TABLE untouched (id INTEGER PRIMARY KEY); INSERT INTO changed VALUES (1); ANALYZE", &[]).unwrap();
    let first = writer.new_session().unwrap();
    let second = writer.new_session().unwrap();
    pause_automatic_statistics(&first);
    pause_automatic_statistics(&second);
    let first_table = first.require_table("changed").unwrap();
    let second_table = second.require_table("changed").unwrap();
    let unrelated_stats = first
        .require_table("untouched")
        .unwrap()
        .column_stats
        .snapshot();
    writer
        .sql("INSERT INTO changed VALUES (2); ANALYZE changed", &[])
        .unwrap();
    first.sql("SELECT * FROM changed", &[]).unwrap();
    second.sql("SELECT * FROM changed", &[]).unwrap();
    assert!(Arc::ptr_eq(
        &first_table,
        &first.require_table("changed").unwrap()
    ));
    assert!(Arc::ptr_eq(
        &second_table,
        &second.require_table("changed").unwrap()
    ));
    assert_eq!(first_table.column_stats.read()["id"].row_count, 2);
    assert!(Arc::ptr_eq(
        &first_table.column_stats.snapshot(),
        &second_table.column_stats.snapshot()
    ));
    assert!(Arc::ptr_eq(
        &unrelated_stats,
        &first
            .require_table("untouched")
            .unwrap()
            .column_stats
            .snapshot()
    ));
}

#[test]
fn independent_engine_refresh_observes_data_and_ddl_without_rebuilding_stable_tables() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("independent-refresh.db");
    let writer = Engine::open(&path).unwrap();
    pause_automatic_statistics(&writer);
    writer
        .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let reader = Engine::open(&path).unwrap();
    pause_automatic_statistics(&reader);
    reader.sql("SELECT * FROM items", &[]).unwrap();
    let before = reader.require_table("items").unwrap();
    writer.sql("INSERT INTO items VALUES (1)", &[]).unwrap();
    assert_eq!(
        reader.sql("SELECT * FROM items", &[]).unwrap().rows.len(),
        1
    );
    assert!(Arc::ptr_eq(
        &before,
        &reader.require_table("items").unwrap()
    ));
    writer
        .sql("ALTER TABLE items ADD COLUMN extra TEXT", &[])
        .unwrap();
    assert!(reader.sql("SELECT extra FROM items", &[]).is_ok());
    writer
        .sql(
            "BEGIN; ALTER TABLE items ADD COLUMN private TEXT; ROLLBACK",
            &[],
        )
        .unwrap();
    assert!(reader.sql("SELECT private FROM items", &[]).is_err());
}

#[test]
fn automatic_statistics_and_concurrent_readers_retain_table_definitions() {
    use std::time::{Duration, Instant};

    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("automatic-refresh.db")).unwrap();
    graph_fixture(&writer, "statistics_unrelated_graph", 1, 100);
    writer
        .sql(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
    writer.begin().unwrap();
    for id in 0..80 {
        writer
            .sql(
                "INSERT INTO items VALUES ($1, 'sample body')",
                &[uqa_sql::SQLParam::Scalar(uqa_core::Value::Int(id))],
            )
            .unwrap();
    }
    writer.commit().unwrap();
    let readers = (0..4)
        .map(|_| writer.new_session().unwrap())
        .collect::<Vec<_>>();
    let completed_before = writer.automatic_statistics_status().completed;
    std::thread::scope(|scope| {
        let tasks = readers
            .iter()
            .map(|reader| {
                scope.spawn(move || {
                    let table = reader.require_table("items").unwrap();
                    let columns = table.columns.snapshot();
                    let graphs = reader.durable.graphs.snapshot();
                    let deadline = Instant::now() + Duration::from_secs(20);
                    loop {
                        let result = reader.sql("SELECT count(*) AS n FROM items", &[]).unwrap();
                        assert_eq!(result.rows[0]["n"], uqa_core::Value::Int(80));
                        let current = reader.require_table("items").unwrap();
                        assert!(Arc::ptr_eq(&table, &current));
                        assert!(Arc::ptr_eq(&columns, &current.columns.snapshot()));
                        assert!(
                            Arc::ptr_eq(&graphs, &reader.durable.graphs.snapshot()),
                            "automatic statistics rebuilt an unrelated graph"
                        );
                        if current
                            .column_stats
                            .read()
                            .get("id")
                            .is_some_and(|stats| stats.row_count == 80)
                        {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "automatic statistics did not refresh: {:?}",
                            reader.automatic_statistics_status()
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
            })
            .collect::<Vec<_>>();
        for task in tasks {
            task.join().unwrap();
        }
    });
    assert!(writer.automatic_statistics_status().last_error.is_none());
    assert!(writer.automatic_statistics_status().completed >= completed_before);
}

fn graph_fixture(engine: &Engine, name: &str, first_id: u64, size: u64) {
    use uqa_graph::GraphStore as _;
    engine.create_graph(name).unwrap();
    engine
        .graph_with_mut(name, |graph| {
            for id in first_id..first_id + size {
                graph.add_vertex(
                    uqa_core::Vertex {
                        vertex_id: id,
                        label: "Item".into(),
                        properties: std::collections::BTreeMap::from([(
                            "body".into(),
                            uqa_core::Value::Str("synthetic graph property".repeat(16)),
                        )]),
                    },
                    name,
                )?;
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn unrelated_registry_writes_preserve_graph_allocations() {
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("registry-graphs.db")).unwrap();
    pause_automatic_statistics(&writer);
    graph_fixture(&writer, "retained_graph", 1, 100);
    writer
        .sql(
            "CREATE TABLE events (id INTEGER PRIMARY KEY); INSERT INTO events VALUES (1)",
            &[],
        )
        .unwrap();
    let reader = writer.new_session().unwrap();
    pause_automatic_statistics(&reader);
    reader.sql("SELECT * FROM events", &[]).unwrap();
    let graphs = reader.durable.graphs.snapshot();
    for value in 0..10 {
        writer
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .set_metadata("unrelated-test-metadata", &value.to_string())
            .unwrap();
        reader.sql("SELECT * FROM events", &[]).unwrap();
        assert!(
            Arc::ptr_eq(&graphs, &reader.durable.graphs.snapshot()),
            "non-graph registry update rebuilt graphs"
        );
    }
    writer
        .sql("CREATE TABLE extra (id INTEGER); ANALYZE events", &[])
        .unwrap();
    reader.sql("SELECT * FROM extra", &[]).unwrap();
    assert!(Arc::ptr_eq(&graphs, &reader.durable.graphs.snapshot()));
    assert!(graphs
        .values()
        .all(|store| matches!(store.as_ref(), uqa_graph::GraphStoreHandle::Persistent(_))));
}

#[test]
fn concurrent_sessions_use_independent_handles_without_rebuilding_graphs() {
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("graph-sharing.db")).unwrap();
    pause_automatic_statistics(&writer);
    graph_fixture(&writer, "changed_graph", 1, 100);
    graph_fixture(&writer, "untouched_graph", 1000, 100);
    writer
        .sql("CREATE TABLE jobs (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let readers = (0..4)
        .map(|_| {
            let reader = writer.new_session().unwrap();
            pause_automatic_statistics(&reader);
            reader.sql("SELECT * FROM jobs", &[]).unwrap();
            reader
        })
        .collect::<Vec<_>>();
    let handles = readers
        .iter()
        .map(|reader| reader.durable.graphs.snapshot())
        .collect::<Vec<_>>();
    for pair in handles.windows(2) {
        assert!(
            !Arc::ptr_eq(&pair[0]["changed_graph"], &pair[1]["changed_graph"]),
            "graph handles must bind to independent physical sessions"
        );
    }
    writer
        .add_graph_vertex(uqa_core::Vertex::new(500, "Item"), "changed_graph")
        .unwrap();
    let barrier = std::sync::Barrier::new(readers.len());
    std::thread::scope(|scope| {
        let tasks = readers
            .iter()
            .map(|reader| {
                scope.spawn(|| {
                    barrier.wait();
                    reader.sql("SELECT * FROM jobs", &[]).unwrap();
                    reader
                        .graph_with("changed_graph", |graph| {
                            use uqa_graph::GraphStore as _;
                            assert_eq!(
                                graph.vertices_in_graph("changed_graph").unwrap().len(),
                                101
                            );
                        })
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for task in tasks {
            task.join().unwrap();
        }
    });
    for (reader, before) in readers.iter().zip(handles) {
        let after = reader.durable.graphs.snapshot();
        assert!(
            Arc::ptr_eq(&before, &after),
            "entity writes rebuilt the graph handle catalog"
        );
        assert!(after
            .values()
            .all(|store| matches!(store.as_ref(), uqa_graph::GraphStoreHandle::Persistent(_))));
    }
}

#[test]
fn graph_edge_label_filter_does_not_load_unrelated_payloads() {
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("filtered-graph.db");
        let engine = if compressed {
            Engine::open_compressed(
                &path,
                uqa_storage_sqlite::SQLiteCompressionOptions::default(),
            )
        } else {
            Engine::open(&path)
        }
        .unwrap();
        pause_automatic_statistics(&engine);
        graph_fixture(&engine, "scoped", 1, 2);
        engine
            .add_graph_edge(uqa_core::Edge::new(1, 1, 2, "selected"), "scoped")
            .unwrap();
        let catalog = engine.storage.catalog.as_ref().unwrap();
        catalog
            .save_edge(99, 1, 2, "unrelated", "invalid-json")
            .unwrap();
        catalog.save_graph_membership("edge", 99, "scoped").unwrap();
        engine
            .sql(
                "CREATE TABLE seeds (id INTEGER PRIMARY KEY); INSERT INTO seeds VALUES (1), (99)",
                &[],
            )
            .unwrap();
        let result = engine
            .sql(
                "SELECT id FROM seeds WHERE graph_edges('scoped', 'selected') ORDER BY id",
                &[],
            )
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["id"], uqa_core::Value::Int(1));
        assert!(engine
            .sql(
                "SELECT id FROM seeds WHERE graph_edges('scoped', 'unrelated')",
                &[]
            )
            .is_err());
        assert!(engine
            .sql("SELECT id FROM seeds WHERE graph_edges('scoped')", &[])
            .is_err());
    }
}

#[test]
fn opening_sessions_and_querying_one_vertex_do_not_hydrate_other_payloads() {
    use uqa_graph::GraphStore as _;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("direct-graphs.db");
    let writer = Engine::open(&path).unwrap();
    pause_automatic_statistics(&writer);
    graph_fixture(&writer, "test_graph", 1, 2);
    let catalog = writer.storage.catalog.as_ref().unwrap();
    catalog
        .save_vertex(99, "Unrelated", "invalid-json")
        .unwrap();
    catalog
        .save_graph_membership("vertex", 99, "test_graph")
        .unwrap();
    for reader in [writer.new_session().unwrap(), Engine::open(&path).unwrap()] {
        pause_automatic_statistics(&reader);
        let handle = reader.durable.graphs.read()["test_graph"].clone();
        assert!(matches!(
            handle.as_ref(),
            uqa_graph::GraphStoreHandle::Persistent(_)
        ));
        assert_eq!(
            reader
                .graph_with("test_graph", |store| store
                    .get_vertex(1)
                    .unwrap()
                    .unwrap()
                    .label)
                .unwrap(),
            Some("Item".into())
        );
        assert!(reader
            .graph_with("test_graph", |store| store.get_vertex(99))
            .unwrap()
            .unwrap()
            .is_err());
        catalog.save_vertex(1, "Changed", "{}").unwrap();
        assert_eq!(
            reader
                .graph_with("test_graph", |store| store
                    .get_vertex(1)
                    .unwrap()
                    .unwrap()
                    .label)
                .unwrap(),
            Some("Changed".into())
        );
        assert!(Arc::ptr_eq(
            &handle,
            &reader.durable.graphs.read()["test_graph"]
        ));
        catalog.save_vertex(1, "Item", "{}").unwrap();
    }
}

#[test]
fn graph_refresh_preserves_transaction_snapshots_and_rollback() {
    use uqa_graph::GraphStore as _;
    let directory = tempfile::tempdir().unwrap();
    let writer = Engine::open(&directory.path().join("graph-rollback.db")).unwrap();
    pause_automatic_statistics(&writer);
    graph_fixture(&writer, "versioned_graph", 1, 1);
    let reader = writer.new_session().unwrap();
    pause_automatic_statistics(&reader);
    reader
        .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
        .unwrap();
    assert_eq!(
        reader
            .graph_with("versioned_graph", |graph| graph
                .vertices_in_graph("versioned_graph")
                .unwrap()
                .len())
            .unwrap(),
        Some(1)
    );
    writer
        .add_graph_vertex(uqa_core::Vertex::new(2, "Item"), "versioned_graph")
        .unwrap();
    assert_eq!(
        reader
            .graph_with("versioned_graph", |graph| graph
                .vertices_in_graph("versioned_graph")
                .unwrap()
                .len())
            .unwrap(),
        Some(1)
    );
    reader.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        reader
            .graph_with("versioned_graph", |graph| graph
                .vertices_in_graph("versioned_graph")
                .unwrap()
                .len())
            .unwrap(),
        Some(2)
    );
    writer.sql("BEGIN; SAVEPOINT graph_change", &[]).unwrap();
    writer
        .add_graph_vertex(uqa_core::Vertex::new(3, "Item"), "versioned_graph")
        .unwrap();
    writer.sql("ROLLBACK TO graph_change; COMMIT", &[]).unwrap();
    assert_eq!(
        reader
            .graph_with("versioned_graph", |graph| graph
                .vertices_in_graph("versioned_graph")
                .unwrap()
                .len())
            .unwrap(),
        Some(2)
    );
    writer
        .add_graph_vertex(uqa_core::Vertex::new(4, "Item"), "versioned_graph")
        .unwrap();
    let ids = reader
        .graph_with("versioned_graph", |graph| {
            graph.vertex_ids_in_graph("versioned_graph").unwrap()
        })
        .unwrap()
        .unwrap();
    assert!(!ids.contains(&3));
    assert!(ids.contains(&4));
    writer.drop_graph("versioned_graph").unwrap();
    assert!(!reader.has_graph("versioned_graph").unwrap());
    graph_fixture(&writer, "versioned_graph", 10, 1);
    assert_eq!(
        reader
            .graph_with("versioned_graph", |graph| graph
                .vertex_ids_in_graph("versioned_graph")
                .unwrap())
            .unwrap()
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![10]
    );
}

#[test]
fn path_index_lookup_as_first_read_establishes_the_fixed_graph_snapshot() {
    use uqa_graph::GraphStore as _;
    for compressed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("path-index-snapshot.db");
        let writer = if compressed {
            Engine::open_compressed(
                &path,
                uqa_storage_sqlite::SQLiteCompressionOptions::default(),
            )
        } else {
            Engine::open(&path)
        }
        .unwrap();
        pause_automatic_statistics(&writer);
        graph_fixture(&writer, "items", 1, 3);
        writer
            .add_graph_edge(uqa_core::Edge::new(1, 1, 2, "next"), "items")
            .unwrap();
        let sequence = vec!["next".to_string()];
        writer
            .build_path_index("paths", "items", std::slice::from_ref(&sequence))
            .unwrap();
        let reader = writer.new_session().unwrap();
        pause_automatic_statistics(&reader);
        reader
            .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
            .unwrap();
        let index = reader.get_path_index("paths", "items").unwrap().unwrap();
        assert!(reader.session.state.read().graph_overlay.is_some());
        writer
            .add_graph_edge(uqa_core::Edge::new(2, 2, 3, "next"), "items")
            .unwrap();
        let expected = std::collections::BTreeSet::from([(1, 2)]);
        assert_eq!(index.lookup(&sequence).unwrap(), Some(expected.clone()));
        // The writer invalidates the live index registration. The escaped
        // read handle and subsequent graph reads still use the fixed data view.
        assert_eq!(
            reader
                .graph_with("items", |store| store
                    .edges_in_graph("items")
                    .unwrap()
                    .len())
                .unwrap(),
            Some(1)
        );
        assert_eq!(index.lookup(&sequence).unwrap(), Some(expected));
        reader.sql("COMMIT", &[]).unwrap();
    }
}

#[test]
fn external_entity_changes_refresh_all_owners_and_dependent_path_indexes() {
    use uqa_graph::GraphStore as _;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared-entities.db");
    let writer = Engine::open(&path).unwrap();
    pause_automatic_statistics(&writer);
    graph_fixture(&writer, "first_graph", 1, 2);
    graph_fixture(&writer, "second_graph", 1, 2);
    let catalog = writer.storage.catalog.as_ref().unwrap();
    catalog.save_edge(10, 1, 2, "LINK", "{}").unwrap();
    for name in ["first_graph", "second_graph"] {
        catalog.save_graph_membership("edge", 10, name).unwrap();
        writer
            .build_path_index("links", name, &[vec!["LINK".into()]])
            .unwrap();
    }
    let reader = Engine::open(&path).unwrap();
    pause_automatic_statistics(&reader);
    // Raw catalog updates do not publish an engine epoch. Storage revisions
    // must invalidate both graph owners and their derived path indexes.
    reader
        .sql("SELECT 'public'::regnamespace::text AS namespace", &[])
        .unwrap();
    assert!(reader.runtime.regtype_output_cache.is_populated());
    catalog.save_vertex(1, "Item", r#"{"value":9}"#).unwrap();
    catalog.save_edge(10, 2, 1, "LINK", "{}").unwrap();
    for name in ["first_graph", "second_graph"] {
        reader
            .graph_with(name, |graph| {
                let vertices = graph.vertices_in_graph(name).unwrap();
                let vertex = vertices
                    .iter()
                    .find(|vertex| vertex.vertex_id == 1)
                    .unwrap();
                assert_eq!(vertex.properties["value"], uqa_core::Value::Int(9));
                let edges = graph.edges_in_graph(name).unwrap();
                assert_eq!((edges[0].source_id, edges[0].target_id), (2, 1));
            })
            .unwrap()
            .unwrap();
        assert!(
            !reader.runtime.regtype_output_cache.is_populated(),
            "graph catalog refresh left stale namespace/relation output names"
        );
        let index = reader.get_path_index("links", name).unwrap().unwrap();
        let paths = index.lookup(&["LINK".into()]).unwrap().unwrap();
        assert!(paths.contains(&(2, 1)));
        assert!(!paths.contains(&(1, 2)));
    }
}
