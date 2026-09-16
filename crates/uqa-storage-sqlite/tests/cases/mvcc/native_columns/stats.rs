//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics replacement rejects incomplete batches and preserves retained views and exact table names.

use super::*;

#[test]
fn native_statistics_replace_preserves_snapshots_and_rejects_incomplete_batches() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    catalog
        .save_column_stats(statistic("docs", "raw", 7))
        .unwrap();
    catalog
        .save_column_stats(statistic(TABLE, "old", 1))
        .unwrap();
    let other = connection.new_session();
    let old = Catalog::open(other.clone()).unwrap();
    other.begin_transaction().unwrap();
    assert_eq!(old.load_column_stats(TABLE).unwrap()[0].column_name, "old");
    connection.begin_transaction().unwrap();
    let a = statistic(TABLE, "a", 2);
    assert!(catalog.replace_column_stats(TABLE, &[a, a]).is_err());
    assert!(catalog
        .replace_column_stats(TABLE, &[a, statistic("docs", "b", 3)])
        .is_err());
    let huge = "x".repeat(1024 * 1024);
    let mut oversized = statistic(TABLE, "huge", 3);
    oversized.histogram_json = &huge;
    assert!(catalog
        .replace_column_stats(TABLE, &[a, oversized])
        .is_err());
    assert_eq!(
        catalog.load_column_stats(TABLE).unwrap()[0].column_name,
        "old"
    );
    assert!(catalog
        .save_column_stats(ColumnStatsInput {
            table_name: "failed",
            ..oversized
        })
        .is_err());
    assert!(catalog.load_column_stats("failed").unwrap().is_empty());
    let mut z = statistic(TABLE, "z", 4);
    z.min_value = Some("{\"typed\":true}");
    z.max_value = None;
    z.distinct_count = i64::MIN;
    z.row_count = i64::MAX;
    catalog.replace_column_stats(TABLE, &[z, a]).unwrap();
    let expected = catalog.load_column_stats(TABLE).unwrap();
    assert_eq!(
        expected
            .iter()
            .map(|row| row.column_name.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert_eq!(expected[1].distinct_count, i64::MIN);
    assert_eq!(expected[1].row_count, i64::MAX);
    assert_eq!(expected[1].min_value.as_deref(), z.min_value);
    assert_eq!(expected[1].max_value, None);
    assert_eq!(expected[0].mcv_values_json, "[2]");
    assert_eq!(expected[0].mcv_frequencies_json, "[0.25]");
    connection.savepoint("keep").unwrap();
    catalog.replace_column_stats(TABLE, &[]).unwrap();
    assert!(catalog.load_column_stats(TABLE).unwrap().is_empty());
    connection.rollback_to_savepoint("keep").unwrap();
    connection.commit_transaction().unwrap();
    assert_eq!(old.load_column_stats(TABLE).unwrap()[0].column_name, "old");
    other.rollback_transaction().unwrap();
    assert_eq!(old.load_column_stats(TABLE).unwrap(), expected);
    assert_eq!(
        catalog.load_column_stats("docs").unwrap()[0].column_name,
        "raw"
    );
    catalog.delete_column_stats(TABLE).unwrap();
    assert!(catalog.load_column_stats(TABLE).unwrap().is_empty());
    assert_eq!(catalog.load_column_stats("docs").unwrap().len(), 1);
    catalog.delete_column_stats("absent").unwrap();
    catalog.replace_column_stats("absent", &[]).unwrap();
}

#[test]
fn native_statistics_reject_competing_updates_to_the_same_column() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    catalog.save_column_stats(statistic(TABLE, "n", 1)).unwrap();
    let other = connection.new_session();
    let writer = Catalog::open(other).unwrap();
    connection.begin_transaction().unwrap();
    catalog.save_column_stats(statistic(TABLE, "n", 2)).unwrap();
    writer.save_column_stats(statistic(TABLE, "n", 3)).unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(
        catalog.load_column_stats(TABLE).unwrap()[0].distinct_count,
        3
    );
}
