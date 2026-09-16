//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cache generations follow native committed/private boundaries without serializing independent writers.

use std::collections::BTreeMap;

use super::{native_tables::schema, open, MODES};
use uqa_core::Value;
use uqa_storage::{mvcc::VersionedSessionOptions, ColumnStatsInput, DocumentStore};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteDocumentStore};

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn write(connection: &ManagedConnection, table: &str, id: u64, value: i64) {
    SQLiteDocumentStore::new(connection.clone(), table)
        .put(id, BTreeMap::from([("n".into(), Value::Int(value))]))
        .unwrap();
}

#[test]
fn native_cache_generations_merge_independent_commits_and_reopen_in_every_mode() {
    let directory = tempfile::tempdir().unwrap();
    for mode in MODES {
        let path = directory.path().join(format!("cache-{mode:?}.db"));
        let a = open(mode, &path);
        let first = Catalog::open(a.clone()).unwrap();
        write(&a, "docs", 1, 1);
        write(&a, "docs", 2, 2);
        bind(&a);
        let b = open(mode, &path);
        bind(&b);
        let second = Catalog::open(b.clone()).unwrap();
        let baseline = first.cache_revisions().unwrap();
        a.begin_transaction().unwrap();
        write(&a, "docs", 1, 10);
        let private = first.cache_revisions().unwrap();
        assert_ne!(private.table_data["docs"], baseline.table_data["docs"]);
        assert_eq!(second.cache_revisions().unwrap(), baseline);
        b.begin_transaction().unwrap();
        write(&b, "docs", 2, 20);
        let other_private = second.cache_revisions().unwrap();
        assert_ne!(private.table_data["docs"], other_private.table_data["docs"]);
        b.commit_transaction().unwrap();
        let middle = second.cache_revisions().unwrap();
        assert!(middle.table_data["docs"] > baseline.table_data["docs"]);
        assert_eq!(first.cache_revisions().unwrap(), private);
        a.commit_transaction().unwrap();
        let committed = first.cache_revisions().unwrap();
        assert!(committed.table_data["docs"] > middle.table_data["docs"]);
        assert_ne!(committed, private);
        assert_eq!(committed, second.cache_revisions().unwrap());
        assert_eq!(committed.table_catalog, baseline.table_catalog);
        assert_eq!(committed.registries, baseline.registries);
        drop((a, b, first, second));
        let reopened = open(mode, &path);
        bind(&reopened);
        assert_eq!(
            Catalog::open(reopened).unwrap().cache_revisions().unwrap(),
            committed
        );
    }
}

#[test]
fn native_cache_generations_restore_savepoints_and_distinguish_new_undo_branches() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    let baseline = catalog.cache_revisions().unwrap();
    connection.begin_transaction().unwrap();
    write(&connection, "docs", 1, 1);
    let first = catalog.cache_revisions().unwrap();
    connection.savepoint("keep").unwrap();
    write(&connection, "docs", 1, 2);
    let discarded = catalog.cache_revisions().unwrap();
    assert_ne!(first, discarded);
    connection.rollback_to_savepoint("keep").unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), first);
    write(&connection, "docs", 1, 3);
    let replacement = catalog.cache_revisions().unwrap();
    assert_ne!(replacement, first);
    assert_ne!(replacement, discarded);
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), baseline);
    connection.begin_transaction().unwrap();
    write(&connection, "docs", 1, 4);
    assert_ne!(catalog.cache_revisions().unwrap(), first);
    connection.rollback_transaction().unwrap();
}

#[test]
fn native_cache_scopes_distinguish_catalog_data_statistics_and_registries() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    connection.begin_transaction().unwrap();
    let initial = catalog.cache_revisions().unwrap();
    catalog.save_table(&schema("docs", 1, 1)).unwrap();
    let table = catalog.cache_revisions().unwrap();
    assert_ne!(table.table_catalog, initial.table_catalog);
    write(&connection, "public.docs", 1, 1);
    let data = catalog.cache_revisions().unwrap();
    assert_eq!(data.table_catalog, table.table_catalog);
    assert_eq!(data.registries, table.registries);
    assert_ne!(data.table_data, table.table_data);
    catalog
        .set_metadata("uqa.statistics.maintenance.v1:public.docs", "{}")
        .unwrap();
    let maintenance = catalog.cache_revisions().unwrap();
    assert_eq!(maintenance.table_data, data.table_data);
    assert_eq!(maintenance.registries, data.registries);
    assert_ne!(
        maintenance.statistics_maintenance,
        data.statistics_maintenance
    );
    catalog
        .save_column_stats(ColumnStatsInput {
            table_name: "public.docs",
            column_name: "n",
            distinct_count: 1,
            null_count: 0,
            min_value: None,
            max_value: None,
            row_count: 1,
            histogram_json: "[]",
            mcv_values_json: "[]",
            mcv_frequencies_json: "[]",
        })
        .unwrap();
    let statistics = catalog.cache_revisions().unwrap();
    assert_ne!(statistics.column_statistics, maintenance.column_statistics);
    assert_eq!(statistics.table_data, maintenance.table_data);
    catalog
        .set_metadata("uqa.table_next_id.v1:public.docs", "2")
        .unwrap();
    let identity = catalog.cache_revisions().unwrap();
    assert_ne!(identity.table_data, statistics.table_data);
    assert_eq!(identity.column_statistics, statistics.column_statistics);
    catalog.save_scoring_params("model", "{}").unwrap();
    let registry = catalog.cache_revisions().unwrap();
    assert_ne!(registry.registries, identity.registries);
    assert_eq!(registry.table_catalog, identity.table_catalog);
    assert_eq!(registry.table_data, identity.table_data);
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), initial);
}

#[test]
fn native_cache_generations_track_graph_owners_without_path_payloads_or_sql_registries() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    for graph in ["g", "other"] {
        catalog.save_named_graph(graph).unwrap();
    }
    catalog.save_vertex(1, "node", "{}").unwrap();
    catalog.save_graph_membership("vertex", 1, "g").unwrap();
    catalog.save_path_index("paths", "[]").unwrap();
    let baseline = catalog.cache_revisions().unwrap();
    connection.begin_transaction().unwrap();
    catalog.save_vertex(1, "node", "{\"n\":1}").unwrap();
    let first = catalog.cache_revisions().unwrap();
    assert_ne!(
        first.graphs.as_ref().unwrap()["g"],
        baseline.graphs.as_ref().unwrap()["g"]
    );
    assert_eq!(
        first.graphs.as_ref().unwrap()["other"],
        baseline.graphs.as_ref().unwrap()["other"]
    );
    assert_eq!(first.registries, baseline.registries);
    catalog
        .save_path_index_pairs("paths", "[]", &[(1, 2)])
        .unwrap();
    catalog.finish_path_index_data("paths", "g", "[]").unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), first);
    connection.savepoint("keep").unwrap();
    catalog.delete_graph_membership("vertex", 1, "g").unwrap();
    let detached = catalog.cache_revisions().unwrap();
    assert_ne!(detached.graphs, first.graphs);
    catalog.save_graph_membership("vertex", 1, "other").unwrap();
    let attached = catalog.cache_revisions().unwrap();
    assert_ne!(
        attached.graphs.as_ref().unwrap()["other"],
        detached.graphs.as_ref().unwrap()["other"]
    );
    catalog
        .set_metadata("graph_label_registry::g", "{}")
        .unwrap();
    assert_eq!(
        catalog.cache_revisions().unwrap().registries,
        first.registries
    );
    connection.rollback_to_savepoint("keep").unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), first);
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), baseline);
}

#[test]
fn native_cache_generations_follow_old_and_new_table_names_and_large_payload_deletion() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_table(&schema("before", 1, 1)).unwrap();
    SQLiteDocumentStore::new(connection.clone(), "public.before")
        .put(
            1,
            BTreeMap::from([("blob".into(), Value::Bytes(vec![9; 1 << 20]))]),
        )
        .unwrap();
    bind(&connection);
    let baseline = catalog.cache_revisions().unwrap();
    connection.begin_transaction().unwrap();
    catalog
        .rename_table_data("public.before", "public.after")
        .unwrap();
    let renamed = catalog.cache_revisions().unwrap();
    assert_ne!(
        renamed.table_data["public.before"],
        baseline.table_data["public.before"]
    );
    assert!(renamed.table_data.contains_key("public.after"));
    connection.savepoint("renamed").unwrap();
    catalog.purge_table_data("public.after").unwrap();
    assert_ne!(
        catalog.cache_revisions().unwrap().table_data["public.after"],
        renamed.table_data["public.after"]
    );
    connection.rollback_to_savepoint("renamed").unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), renamed);
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), baseline);
}

#[test]
fn native_cache_reads_and_failed_batches_stay_bounded_beside_unrelated_payloads() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    let payload = "x".repeat(1 << 20);
    catalog.save_scoring_params("large", &payload).unwrap();
    catalog.save_vertex(999, "unrelated", &payload).unwrap();
    write(&connection, "docs", 1, 1);
    uqa_storage_sqlite::SQLiteRecordStore::for_native(
        &connection,
        &uqa_storage::read_control::StorageReadControl::with_limit(16 << 20),
    )
    .unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 128 << 10,
        })
        .unwrap();
    let baseline = catalog.cache_revisions().unwrap();
    connection.begin_transaction().unwrap();
    write(&connection, "docs", 1, 2);
    let private = catalog.cache_revisions().unwrap();
    assert_ne!(private.table_data, baseline.table_data);
    assert!(catalog.set_metadata("oversized", &payload).is_err());
    assert_eq!(catalog.cache_revisions().unwrap(), private);
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.cache_revisions().unwrap(), baseline);
}

#[test]
fn native_cache_generation_collections_respect_the_complete_read_allowance() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("many-cache-scopes.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection.begin_transaction().unwrap();
    for id in 0..200 {
        catalog
            .save_named_graph(&format!("{id}:{}", "g".repeat(512)))
            .unwrap();
    }
    connection.commit_transaction().unwrap();
    bind(&connection);
    let baseline = catalog.cache_revisions().unwrap();
    let small = ManagedConnection::open(&path).unwrap();
    small
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 64 << 10,
        })
        .unwrap();
    assert!(Catalog::open(small).unwrap().cache_revisions().is_err());
    assert_eq!(catalog.cache_revisions().unwrap(), baseline);
}
