//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual field lifetimes bound coordination records after catalog deletion and reader release.

use std::sync::Arc;

use super::{expect, expect_eq, values};
use crate::key_value::{KeyValueCatalog, KeyValueDiskANNCanonical, KeyValueVectorFieldGuards};
use crate::mvcc::{VectorFieldGuardLayout, VersionError};
use crate::read_control::StorageReadControl;
use crate::{
    CatalogFacade, KeyValueStore, RelationIdentity, RelationSecurityRow, StorageBackendResult,
    TableSchema, VectorFieldSchema,
};

/// Check three distinct table lifetimes, nonempty fields, retained readers and a writer racing empty-field cleanup. No `DiskANN` graph marker is required.
pub fn verify_vector_field_guard_reclamation(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public")?;
    let prefix = KeyValueVectorFieldGuards
        .prefix(&control)
        .map_err(VersionError::into_storage_error)?;
    let neighbor = b"\0uqa-vector-field-guards-v1-neighbor\0";
    store.put(neighbor, b"preserved")?;
    for cycle in 1..=3 {
        let row = TableSchema {
            relation: RelationIdentity::new("public", format!("guard_lifetime_{cycle}")),
            security: RelationSecurityRow::legacy("owner"),
            object_id: [cycle; 16],
            storage_generation: [cycle; 16],
            analyzer_json: "{}".into(),
            fts_fields: vec![],
            vector_fields: vec![VectorFieldSchema {
                field: "embedding\0日本語".into(),
                dimensions: 2,
            }],
            columns_json: "[]".into(),
            constraints_json: "{}".into(),
        };
        catalog.save_table(&row)?;
        let table = row.relation.qualified_name();
        let canonical =
            KeyValueDiskANNCanonical::new(store.clone(), &table, "embedding\0日本語", 2)?;
        canonical.replace(1, &[vec![1.0, -0.0]], &control)?;
        let retained = canonical.retain(&control)?;
        let held_records = store.open_retained_read_session(control.cancellation())?;
        let keys = store.scan_prefix(&prefix)?;
        expect_eq(
            &keys.len(),
            &1,
            "one reference marker for the current field",
        )?;
        let guard = KeyValueVectorFieldGuards
            .reference(&keys[0].0, &control)
            .map_err(VersionError::into_storage_error)?
            .unwrap();
        store.reclaim_obsolete()?;
        expect(
            store.get(&guard.references)?.is_some(),
            "nonempty field retains its marker",
        )?;
        catalog.drop_table_and_data(&table)?;
        store.reclaim_obsolete()?;
        expect(
            store.get(&guard.references)?.is_none(),
            "dropped field releases its live reference marker",
        )?;
        values(&retained, 1, &[vec![1.0, -0.0]], &control)?;
        expect_eq(
            &held_records.get(&guard.references)?,
            &Some(vec![1]),
            "old reader keeps the original reference marker",
        )?;
        drop(retained);
        drop(held_records);
        store.reclaim_obsolete()?;
        expect_eq(
            &store.scan_prefix(&prefix)?.len(),
            &0,
            "guard count does not grow across table lifetimes",
        )?;
    }
    let empty = KeyValueDiskANNCanonical::new(store.clone(), "guard-race", "embedding", 2)?;
    empty.replace(0, &[], &control)?;
    let peer = store.open_session()?;
    let writer = KeyValueDiskANNCanonical::new(peer.clone(), "guard-race", "embedding", 2)?;
    peer.begin_transaction()?;
    writer.replace(1, &[vec![1.0, 0.0]], &control)?;
    store.reclaim_obsolete()?;
    peer.commit_transaction()?;
    store.reclaim_obsolete()?;
    values(&empty.retain(&control)?, 1, &[vec![1.0, 0.0]], &control)?;
    expect_eq(
        &store.get(neighbor)?,
        &Some(b"preserved".to_vec()),
        "neighboring namespace survives",
    )
}
