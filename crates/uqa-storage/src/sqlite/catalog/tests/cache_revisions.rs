//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
