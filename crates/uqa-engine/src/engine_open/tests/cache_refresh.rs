//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::Engine;

fn pause_automatic_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
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
    let loads = reader.row_locks.graph_snapshots.load_count();
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
    assert_eq!(reader.row_locks.graph_snapshots.load_count(), loads);
}

#[test]
fn concurrent_sessions_restore_only_the_changed_graph_once() {
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
    let untouched = readers[0].durable.graphs.read()["untouched_graph"].clone();
    writer
        .add_graph_vertex(uqa_core::Vertex::new(500, "Item"), "changed_graph")
        .unwrap();
    let loads = writer.row_locks.graph_snapshots.load_count();
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
    assert_eq!(
        writer.row_locks.graph_snapshots.load_count() - loads,
        1,
        "each reader decoded the same graph version independently"
    );
    let changed = readers[0].durable.graphs.read()["changed_graph"].clone();
    for reader in readers {
        assert!(Arc::ptr_eq(
            &changed,
            &reader.durable.graphs.read()["changed_graph"]
        ));
        assert!(Arc::ptr_eq(
            &untouched,
            &reader.durable.graphs.read()["untouched_graph"]
        ));
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
    assert!(reader.runtime.regtype_output_cache.lock().is_some());
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
            reader.runtime.regtype_output_cache.lock().is_none(),
            "graph catalog refresh left stale namespace/relation output names"
        );
        let index = reader.get_path_index("links", name).unwrap().unwrap();
        let paths = index.lookup(&["LINK".into()]).unwrap();
        assert!(paths.contains(&(2, 1)));
        assert!(!paths.contains(&(1, 2)));
    }
}
