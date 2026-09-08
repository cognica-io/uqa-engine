//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn graph_revision_migration_upgrades_existing_graphs_and_legacy_triggers() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_named_graph("existing_graph").unwrap();
    catalog.save_vertex(1, "Item", r#"{"value":1}"#).unwrap();
    catalog
        .save_graph_membership("vertex", 1, "existing_graph")
        .unwrap();
    connection.with(|conn| {
        // Reproduce the old graph/metadata triggers, which all invalidated
        // the registry instead of tracking the graph owning the mutation.
        for table in ["_named_graphs", "_graph_vertices", "_graph_edges", "_graph_membership", "_metadata"] {
            for event in ["INSERT", "DELETE", "UPDATE"] {
                let trigger = format!("uqa_cache_{table}_{event}");
                conn.execute_batch(&format!(
                    "DROP TRIGGER {trigger}; CREATE TRIGGER {trigger} AFTER {event} ON {table} BEGIN \
                     INSERT INTO _cache_revisions(kind, name, generation) VALUES ('registry', '', 1) \
                     ON CONFLICT(kind, name) DO UPDATE SET generation = generation + 1; END;"
                ))?;
            }
        }
        conn.execute_batch("DROP INDEX _graph_membership_by_graph; DELETE FROM _cache_revisions WHERE kind = 'graph'; UPDATE _metadata SET value = '44' WHERE key = 'schema_version';")?;
        Ok(())
    }).unwrap();
    drop(catalog);
    let upgraded = Catalog::open(connection.clone()).unwrap();
    assert_eq!(
        upgraded
            .load_named_graph_snapshot("existing_graph")
            .unwrap()
            .unwrap()
            .vertices
            .len(),
        1
    );
    let before = upgraded.cache_revisions().unwrap();
    assert!(before
        .graphs
        .as_ref()
        .unwrap()
        .contains_key("existing_graph"));
    upgraded.save_vertex(1, "Item", r#"{"value":2}"#).unwrap();
    let after = upgraded.cache_revisions().unwrap();
    assert_eq!(before.registries, after.registries);
    assert!(
        after.graphs.as_ref().unwrap()["existing_graph"]
            > before.graphs.as_ref().unwrap()["existing_graph"]
    );
    upgraded
        .set_metadata("graph_label_registry::existing_graph", "{}")
        .unwrap();
    assert_eq!(
        upgraded.cache_revisions().unwrap().registries,
        after.registries
    );
    let after = upgraded.cache_revisions().unwrap();
    drop(upgraded);
    let reopened = Catalog::open(connection).unwrap();
    assert_eq!(
        reopened.cache_revisions().unwrap(),
        after,
        "reopening reran the migration"
    );
}

#[test]
fn cache_revisions_distinguish_data_statistics_and_definitions() {
    let catalog = fresh();
    catalog.save_table(&empty_table("public", "items")).unwrap();
    let before = catalog.cache_revisions().unwrap();
    catalog
        .conn
        .with(|conn| {
            conn.execute(
                "INSERT INTO _documents(table_name, doc_id, body) VALUES ('public.items', 1, '{}')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let data = catalog.cache_revisions().unwrap();
    assert_eq!(before.table_catalog, data.table_catalog);
    assert_eq!(before.registries, data.registries);
    assert_eq!(before.column_statistics, data.column_statistics);
    assert_ne!(before.table_data, data.table_data);
    catalog
        .set_metadata("uqa.statistics.maintenance.v1:public.items", "{}")
        .unwrap();
    let maintenance = catalog.cache_revisions().unwrap();
    assert_eq!(data.table_data, maintenance.table_data);
    assert_eq!(data.registries, maintenance.registries);
    assert_ne!(
        data.statistics_maintenance,
        maintenance.statistics_maintenance
    );
    catalog.conn.with(|conn| {
        conn.execute("INSERT INTO _column_stats(table_name, column_name, distinct_count, null_count, row_count) VALUES ('public.items', 'id', 1, 0, 1)", [])?;
        Ok(())
    }).unwrap();
    let stats = catalog.cache_revisions().unwrap();
    assert_eq!(maintenance.table_catalog, stats.table_catalog);
    assert_eq!(maintenance.registries, stats.registries);
    assert_eq!(maintenance.table_data, stats.table_data);
    assert_ne!(maintenance.column_statistics, stats.column_statistics);
}

#[test]
fn cache_revisions_follow_transaction_and_savepoint_visibility() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("revisions.db")).unwrap();
    let writer = Catalog::open(connection.clone()).unwrap();
    let observer = Catalog::open(connection.new_session()).unwrap();
    let before = observer.cache_revisions().unwrap();
    connection.begin_transaction().unwrap();
    writer
        .save_table(&empty_table("public", "pending"))
        .unwrap();
    assert_ne!(writer.cache_revisions().unwrap(), before);
    assert_eq!(observer.cache_revisions().unwrap(), before);
    connection.rollback_transaction().unwrap();
    assert_eq!(writer.cache_revisions().unwrap(), before);
    connection.begin_transaction().unwrap();
    writer
        .save_table(&empty_table("public", "committed"))
        .unwrap();
    let after_table = writer.cache_revisions().unwrap();
    connection
        .with(|conn| {
            conn.execute_batch("SAVEPOINT checkpoint")
                .map_err(Into::into)
        })
        .unwrap();
    writer.set_metadata("registry-example", "value").unwrap();
    connection
        .with(|conn| {
            conn.execute_batch("ROLLBACK TO checkpoint; RELEASE checkpoint")
                .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(writer.cache_revisions().unwrap(), after_table);
    connection.commit_transaction().unwrap();
    assert_eq!(observer.cache_revisions().unwrap(), after_table);
}

#[test]
fn graph_revisions_track_memberships_shared_entities_and_label_metadata() {
    let catalog = fresh();
    for name in ["first_graph", "second_graph", "untouched_graph"] {
        catalog.save_named_graph(name).unwrap();
    }
    catalog.save_vertex(1, "Item", "{}").unwrap();
    catalog
        .save_graph_membership("vertex", 1, "first_graph")
        .unwrap();
    catalog
        .save_graph_membership("vertex", 1, "second_graph")
        .unwrap();
    let before = catalog.cache_revisions().unwrap();
    catalog.save_vertex(1, "Item", r#"{"value":2}"#).unwrap();
    let after = catalog.cache_revisions().unwrap();
    assert_eq!(before.registries, after.registries);
    let old = before.graphs.unwrap();
    let graphs = after.graphs.unwrap();
    assert!(graphs["first_graph"] > old["first_graph"]);
    assert!(graphs["second_graph"] > old["second_graph"]);
    assert_eq!(graphs["untouched_graph"], old["untouched_graph"]);
    catalog
        .set_metadata("graph_label_registry::first_graph", "{}")
        .unwrap();
    let labels = catalog.cache_revisions().unwrap();
    assert_eq!(labels.registries, after.registries);
    assert!(labels.graphs.as_ref().unwrap()["first_graph"] > graphs["first_graph"]);
    catalog.set_metadata("unrelated-metadata", "value").unwrap();
    assert_eq!(catalog.cache_revisions().unwrap().graphs, labels.graphs);
}

#[test]
fn graph_revisions_follow_rollback_and_survive_drop_recreate() {
    let catalog = fresh();
    catalog.save_named_graph("versioned_graph").unwrap();
    let before = catalog.cache_revisions().unwrap();
    catalog.conn.begin_transaction().unwrap();
    catalog.save_vertex(1, "Item", "{}").unwrap();
    catalog
        .save_graph_membership("vertex", 1, "versioned_graph")
        .unwrap();
    assert_ne!(catalog.cache_revisions().unwrap().graphs, before.graphs);
    catalog.conn.rollback_transaction().unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), before);
    catalog.drop_named_graph_data("versioned_graph").unwrap();
    let dropped = catalog.cache_revisions().unwrap().graphs.unwrap()["versioned_graph"];
    catalog.save_named_graph("versioned_graph").unwrap();
    assert!(catalog.cache_revisions().unwrap().graphs.unwrap()["versioned_graph"] > dropped);
}

#[test]
fn graph_snapshot_load_is_scoped_and_rejects_missing_members() {
    let catalog = fresh();
    catalog.save_named_graph("first_graph").unwrap();
    catalog.save_named_graph("second_graph").unwrap();
    for (id, graph) in [(1, "first_graph"), (2, "second_graph")] {
        catalog.save_vertex(id, "Item", "{}").unwrap();
        catalog.save_graph_membership("vertex", id, graph).unwrap();
    }
    let snapshot = catalog
        .load_named_graph_snapshot("first_graph")
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.vertices.len(), 1);
    assert_eq!(snapshot.vertices[0].vertex_id, 1);
    assert!(snapshot.edges.is_empty());
    catalog.delete_vertex(2).unwrap();
    assert!(catalog
        .load_named_graph_snapshot("first_graph")
        .unwrap()
        .is_some());
    assert!(catalog
        .load_named_graph_snapshot("second_graph")
        .unwrap_err()
        .to_string()
        .contains("missing vertex 2"));
    assert!(catalog
        .load_named_graph_snapshot("absent_graph")
        .unwrap()
        .is_none());
    catalog
        .save_graph_membership("vertex", 1, "absent_graph")
        .unwrap();
    assert!(catalog
        .load_named_graph_snapshot("absent_graph")
        .unwrap_err()
        .to_string()
        .contains("unregistered graph"));
}
