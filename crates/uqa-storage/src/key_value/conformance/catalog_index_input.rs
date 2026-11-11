//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained index definitions remain independent of later catalog transactions.

use super::{expect, expect_eq};
use crate::{
    catalog::input::{CatalogInputLimits, CatalogInputStep},
    read_control::StorageReadControl,
    CatalogIndexRow, PersistentStorageSession, RelationIdentity, StorageBackendResult,
};

/// Verify original names, encoded fields, optional definitions and private membership on a fresh disposable provider session.
pub fn verify_catalog_index_input(source: PersistentStorageSession) -> StorageBackendResult<()> {
    prepare(&source)?;
    let expected = source.catalog.load_catalog_indexes()?;
    let control = StorageReadControl::with_limit(65536);
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 8192,
    };
    let mut input = source.catalog.begin_index_input(limits, &control)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    let mut changed = row("a");
    changed.definition_json = Some("later".into());
    later.catalog.save_catalog_index_row(&changed)?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "index input does not retain the source catalog",
    )?;
    let mut actual = Vec::new();
    let mut pending = 0;
    for _ in 0..32 {
        match input.advance()? {
            CatalogInputStep::Row(row) => actual.push(row),
            CatalogInputStep::Pending => pending += 1,
            CatalogInputStep::Complete => break,
        }
    }
    expect(
        matches!(input.advance()?, CatalogInputStep::Complete),
        "finite catalog input completes",
    )?;
    expect(pending > 0, "deleted index consumes its own record window")?;
    actual.sort_by(|left, right| left.relation.cmp(&right.relation));
    expect_eq(&actual.len(), &expected.len(), "retained index membership")?;
    for (actual, expected) in actual.iter().zip(&expected) {
        equal(actual, expected)?;
    }
    expect_eq(
        &control.memory().used(),
        &actual.iter().map(|row| bytes(row)).sum::<usize>(),
        "completed rows retain exactly their field capacities",
    )?;
    control.cancellation().cancel();
    expect(
        matches!(input.advance()?, CatalogInputStep::Complete),
        "completion remains idempotent",
    )?;
    drop(actual);
    expect_eq(
        &control.memory().used(),
        &0,
        "row retirement releases original admission",
    )?;

    let denied = StorageReadControl::with_limit(65536);
    let mut input = later.catalog.begin_index_input(
        CatalogInputLimits {
            maximum_value_bytes: 1,
            ..limits
        },
        &denied,
    )?;
    expect(
        input.advance().is_err(),
        "encoded index value is checked before decoding",
    )?;
    expect(input.advance().is_err(), "failed index input cannot resume")?;
    expect_eq(
        &denied.memory().used(),
        &0,
        "failure retires the retained view",
    )
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
    for name in ["a", "z", "Index\"Name"] {
        source.catalog.save_catalog_index_row(&row(name))?;
    }
    source.backend.begin_transaction()?;
    let mut changed = row("a");
    changed.definition_json = Some(" { \"n\": -0.0, \"s\": \"\\u0000한글\" } ".into());
    source.catalog.save_catalog_index_row(&changed)?;
    source.catalog.drop_catalog_index(&row("z").relation)?;
    source.catalog.save_catalog_index_row(&row("private"))?;
    Ok(())
}

fn row(name: &str) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("Schema.Name", name),
        index_type: "btree".into(),
        table_name: "\"Schema.Name\".\"Table.Name\"".into(),
        columns_json: "[ \"Field.Name\" ]".into(),
        parameters_json: " { \"n\": 1.00 } ".into(),
        definition_json: None,
    }
}

fn equal(left: &CatalogIndexRow, right: &CatalogIndexRow) -> StorageBackendResult<()> {
    expect_eq(
        &left.relation,
        &right.relation,
        "original relation identity",
    )?;
    expect_eq(&left.index_type, &right.index_type, "original index type")?;
    expect_eq(
        &left.table_name,
        &right.table_name,
        "original backing relation",
    )?;
    expect_eq(
        &left.columns_json,
        &right.columns_json,
        "original encoded columns",
    )?;
    expect_eq(
        &left.parameters_json,
        &right.parameters_json,
        "original encoded parameters",
    )?;
    expect_eq(
        &left.definition_json,
        &right.definition_json,
        "original optional definition",
    )
}

fn bytes(row: &CatalogIndexRow) -> usize {
    row.relation.schema.capacity()
        + row.relation.name.capacity()
        + row.index_type.capacity()
        + row.table_name.capacity()
        + row.columns_json.capacity()
        + row.parameters_json.capacity()
        + row.definition_json.as_ref().map_or(0, String::capacity)
}
