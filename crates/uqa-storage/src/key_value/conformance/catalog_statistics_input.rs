//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent statistics preserve their selected committed/private catalog and original field buffers.

use super::{expect, expect_eq};
use crate::{
    catalog::input::{CatalogInputLimits, CatalogInputStep},
    read_control::StorageReadControl,
    ColumnStatsInput, ColumnStatsRow, PersistentStorageSession, RelationIdentity,
    StorageBackendResult,
};

const TABLE: &str = "\"Schema.Name\".\"Table.Name\"";

/// Exercise one-record input, source retirement, tombstones, bounds, cancellation and exact stored values on a fresh disposable provider.
pub fn verify_catalog_statistics_input(
    source: PersistentStorageSession,
) -> StorageBackendResult<()> {
    prepare(&source)?;
    let mut expected = source.catalog.load_column_stats(TABLE)?;
    expected.sort_by(|a, b| a.column_name.cmp(&b.column_name));
    let control = StorageReadControl::with_limit(65536);
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 8192,
    };
    let mut input = source
        .catalog
        .begin_column_stats_input(TABLE, limits, &control)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later
        .catalog
        .replace_column_stats(TABLE, &[row("later", 999)])?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "statistics input releases the source catalog",
    )?;
    let mut actual = Vec::new();
    let mut pending = 0;
    let mut completed = false;
    for _ in 0..32 {
        match input.advance()? {
            CatalogInputStep::Row(row) => actual.push(row),
            CatalogInputStep::Pending => pending += 1,
            CatalogInputStep::Complete => {
                completed = true;
                break;
            }
        }
    }
    expect(completed, "bounded statistics input completes")?;
    expect(
        pending > 0,
        "deleted statistics consume their own physical slots",
    )?;
    actual.sort_by(|a, b| a.column_name.cmp(&b.column_name));
    expect_eq(
        &actual.len(),
        &expected.len(),
        "retained statistics membership",
    )?;
    for (left, right) in actual.iter().zip(&expected) {
        expect_eq(&**left, right, "exact stored statistics row")?;
    }
    expect_eq(
        &control.memory().used(),
        &actual.iter().map(|row| bytes(row)).sum::<usize>(),
        "original statistics buffer capacities",
    )?;
    control.cancellation().cancel();
    expect(
        matches!(input.advance()?, CatalogInputStep::Complete),
        "completed statistics input remains complete",
    )?;
    drop(actual);
    expect_eq(
        &control.memory().used(),
        &0,
        "statistics retirement releases admission",
    )?;

    verify_failures(&later, limits)
}

fn verify_failures(
    later: &PersistentStorageSession,
    limits: CatalogInputLimits,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(65536);
    let mut input = later.catalog.begin_column_stats_input(
        TABLE,
        CatalogInputLimits {
            maximum_value_bytes: 1,
            ..limits
        },
        &control,
    )?;
    let mut rejected = false;
    for _ in 0..32 {
        match input.advance() {
            Ok(CatalogInputStep::Pending) => (),
            Err(_) => {
                rejected = true;
                break;
            }
            Ok(_) => break,
        }
    }
    expect(rejected, "statistics value size checked before decoding")?;
    expect(
        input.advance().is_err(),
        "failed statistics input cannot resume",
    )?;
    expect_eq(
        &control.memory().used(),
        &0,
        "statistics failure retires original view",
    )?;

    let control = StorageReadControl::with_limit(65536);
    let mut input = later
        .catalog
        .begin_column_stats_input(TABLE, limits, &control)?;
    control.cancellation().cancel();
    expect(
        input.advance().is_err(),
        "original statistics owner cancellation",
    )?;
    expect(
        input.advance().is_err(),
        "cancelled statistics input cannot resume",
    )?;
    expect_eq(
        &control.memory().used(),
        &0,
        "cancelled statistics view is retired",
    )?;

    let control = StorageReadControl::with_limit(65536);
    let mut empty = later
        .catalog
        .begin_column_stats_input("absent", limits, &control)?;
    expect(
        matches!(empty.advance()?, CatalogInputStep::Complete),
        "absent table has no statistics",
    )?;
    expect_eq(
        &control.memory().used(),
        &0,
        "empty statistics release their view",
    )?;
    expect(
        later
            .catalog
            .begin_column_stats_input(
                "absent",
                CatalogInputLimits {
                    maximum_value_bytes: 0,
                    ..limits
                },
                &control,
            )
            .is_err(),
        "empty statistics still validate record limits",
    )?;
    Ok(())
}

fn prepare(source: &PersistentStorageSession) -> StorageBackendResult<()> {
    source.catalog.initialize_storage()?;
    source
        .catalog
        .save_schema_row(&crate::SchemaRow::bootstrap("Schema.Name"))?;
    source.catalog.save_table(&crate::TableSchema {
        relation: RelationIdentity::new("Schema.Name", "Table.Name"),
        security: crate::RelationSecurityRow::bootstrap(),
        object_id: [3; 16],
        storage_generation: [4; 16],
        analyzer_json: "{}".into(),
        fts_fields: vec![],
        vector_fields: vec![],
        columns_json: "[]".into(),
        constraints_json: "{}".into(),
    })?;
    for name in ["a", "z", "Field\"Name"] {
        source.catalog.save_column_stats(row(name, 3))?;
    }
    source.backend.begin_transaction()?;
    source
        .catalog
        .replace_column_stats(TABLE, &[row("a", -7), row("private", i64::MAX)])?;
    Ok(())
}

fn row(column: &str, count: i64) -> ColumnStatsInput<'_> {
    ColumnStatsInput {
        table_name: TABLE,
        column_name: column,
        distinct_count: count,
        null_count: -1,
        min_value: Some(" { \"value\": -0.0 } "),
        max_value: None,
        row_count: i64::MIN,
        histogram_json: " [1.00, \"한글\"] ",
        mcv_values_json: "opaque invalid JSON",
        mcv_frequencies_json: "[0.25]",
    }
}

fn bytes(row: &ColumnStatsRow) -> usize {
    row.column_name.capacity()
        + row.histogram_json.capacity()
        + row.mcv_values_json.capacity()
        + row.mcv_frequencies_json.capacity()
        + row.min_value.as_ref().map_or(0, String::capacity)
        + row.max_value.as_ref().map_or(0, String::capacity)
}
