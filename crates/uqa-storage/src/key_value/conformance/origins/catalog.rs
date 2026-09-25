//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical origins follow existing table and field data lifecycles atomically.

use super::{expect, expect_eq, values};
use crate::diskann_index::format::DiskANNChangeIdentity;
use crate::key_value::vector_index::origin::journal;
use crate::key_value::KeyValueDiskANNCanonical;
use crate::read_control::StorageReadControl;
use crate::{
    CatalogFacade, KeyValueCatalog, KeyValueStore, RelationIdentity, RelationSecurityRow,
    StorageBackendResult, TableSchema,
};
use std::sync::Arc;

fn schema(name: &str) -> TableSchema {
    TableSchema {
        relation: RelationIdentity::new("public", name),
        security: RelationSecurityRow::legacy("owner"),
        object_id: [0; 16],
        storage_generation: [0; 16],
        analyzer_json: "{}".into(),
        fts_fields: vec![],
        vector_fields: vec![],
        columns_json: "[]".into(),
        constraints_json: "{}".into(),
    }
}

pub(super) fn verify(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public")?;
    catalog.save_table(&schema("origin_lifecycle"))?;
    let original =
        KeyValueDiskANNCanonical::new(store.clone(), "public.origin_lifecycle", "before", 2)?;
    let version = original.replace(1, &[vec![1.0, -0.0]], control)?;
    original.replace(2, &[], control)?;
    let retained = original.retain(control)?;
    catalog.rename_column_data("public.origin_lifecycle", "before", "after")?;
    let field =
        KeyValueDiskANNCanonical::new(store.clone(), "public.origin_lifecycle", "after", 2)?;
    expect_eq(
        &field.retain(control)?.origin(1, control)?,
        &Some(version),
        "column rename preserves actual origin",
    )?;
    expect_eq(
        &field.retain(control)?.next_change_after(None, control)?,
        &Some(DiskANNChangeIdentity::new(1, version)),
        "column rename preserves its change namespace",
    )?;
    empty_journal(store, "public.origin_lifecycle", "before", control)?;
    expect(
        original.retain(control)?.origin(1, control)?.is_none(),
        "column rename retires old origins",
    )?;
    catalog.rename_table_data("public.origin_lifecycle", "public.origin_renamed")?;
    let renamed =
        KeyValueDiskANNCanonical::new(store.clone(), "public.origin_renamed", "after", 2)?;
    expect_eq(
        &renamed.retain(control)?.origin(1, control)?,
        &Some(version),
        "table rename preserves actual origin",
    )?;
    expect_eq(
        &renamed.retain(control)?.next_change_after(None, control)?,
        &Some(DiskANNChangeIdentity::new(1, version)),
        "table rename preserves its change namespace",
    )?;
    empty_journal(store, "public.origin_lifecycle", "after", control)?;
    expect(
        field.retain(control)?.origin(2, control)?.is_none(),
        "table rename retires empty origin records",
    )?;
    values(&retained, 1, &[vec![1.0, -0.0]], control)?;
    occupied_destination(store, &catalog, control)?;
    store.begin_transaction()?;
    catalog.drop_column_data("public.origin_renamed", "after")?;
    expect(
        renamed.retain(control)?.origin(1, control)?.is_none(),
        "column drop removes origins with canonical values",
    )?;
    empty_journal(store, "public.origin_renamed", "after", control)?;
    store.rollback_transaction()?;
    expect_eq(
        &renamed.retain(control)?.origin(1, control)?,
        &Some(version),
        "column drop undo restores the same origin",
    )?;
    expect_eq(
        &renamed.retain(control)?.next_change_after(None, control)?,
        &Some(DiskANNChangeIdentity::new(1, version)),
        "column drop undo restores its original change",
    )?;
    catalog.drop_column_data("public.origin_renamed", "after")?;
    expect(
        renamed.retain(control)?.origin(2, control)?.is_none(),
        "column drop removes zero-count origins",
    )?;
    for drop_table in [false, true] {
        renamed.replace(1, &[vec![2.0, 3.0]], control)?;
        renamed.replace(2, &[], control)?;
        if drop_table {
            catalog.drop_table_and_data("public.origin_renamed")?;
        } else {
            catalog.purge_table_data("public.origin_renamed")?;
        }
        let source = renamed.retain(control)?;
        expect(
            source.origin(1, control)?.is_none() && source.origin(2, control)?.is_none(),
            "table cleanup removes populated and empty origins",
        )?;
        empty_journal(store, "public.origin_renamed", "after", control)?;
    }
    catalog.save_table(&schema("origin_renamed"))?;
    expect(
        renamed.retain(control)?.origin(2, control)?.is_none(),
        "recreated table cannot resurrect old origins",
    )?;
    expect_eq(
        &retained.next_change_after(None, control)?,
        &Some(DiskANNChangeIdentity::new(1, version)),
        "retained changes survive owner cleanup",
    )
}

fn empty_journal(
    store: &Arc<dyn KeyValueStore>,
    table: &str,
    field: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let prefix = journal::prefix(table, field)?;
    store.with_read_view(&mut |read| {
        expect(
            !read.contains_prefix_budgeted(&prefix, control)?,
            "retired change namespace is empty",
        )
    })
}

fn occupied_destination(
    store: &Arc<dyn KeyValueStore>,
    catalog: &KeyValueCatalog,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let destination =
        KeyValueDiskANNCanonical::new(store.clone(), "public.origin_destination", "after", 2)?;
    destination.replace(2, &[], control)?;
    expect(
        catalog
            .rename_table_data("public.origin_renamed", "public.origin_destination")
            .is_err(),
        "conflicting origin-only destination is occupied",
    )
}
