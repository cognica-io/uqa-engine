//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical origins follow existing table and field data lifecycles atomically.

use super::{expect, expect_eq, values};
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
    expect(
        field.retain(control)?.origin(2, control)?.is_none(),
        "table rename retires empty origin records",
    )?;
    values(&retained, 1, &[vec![1.0, -0.0]], control)?;
    let destination =
        KeyValueDiskANNCanonical::new(store.clone(), "public.origin_destination", "after", 2)?;
    destination.replace(2, &[], control)?;
    expect(
        catalog
            .rename_table_data("public.origin_renamed", "public.origin_destination")
            .is_err(),
        "origin-only destination is occupied",
    )?;
    store.begin_transaction()?;
    catalog.drop_column_data("public.origin_renamed", "after")?;
    expect(
        renamed.retain(control)?.origin(1, control)?.is_none(),
        "column drop removes origins with canonical values",
    )?;
    store.rollback_transaction()?;
    expect_eq(
        &renamed.retain(control)?.origin(1, control)?,
        &Some(version),
        "column drop undo restores the same origin",
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
    }
    catalog.save_table(&schema("origin_renamed"))?;
    expect(
        renamed.retain(control)?.origin(2, control)?.is_none(),
        "recreated table cannot resurrect old origins",
    )?;
    Ok(())
}
