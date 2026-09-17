//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concurrent IVF generations agree byte-for-byte with the serial index owner.

use std::sync::Arc;

use super::{expect, expect_eq};
use crate::{
    key_value::{
        codec::vector_field_prefix,
        index_keys::{ivf_assignment_prefix, ivf_centroid_prefix, ivf_metadata_key},
        KeyValueIVFIndex,
    },
    IVFIndexParams, KeyValueStore, MemoryKeyValueStore, StorageBackendResult, VectorIndex,
};

const FIELD: &str = "embedding\0日本語";
fn params() -> IVFIndexParams {
    IVFIndexParams {
        nlist: 2,
        nprobe: 2,
        train_threshold: 3,
    }
}
fn create(
    store: Arc<dyn KeyValueStore>,
    table: &str,
    seed: u64,
) -> StorageBackendResult<KeyValueIVFIndex> {
    let mut index = KeyValueIVFIndex::create(store, table, FIELD, 2, params())?;
    for document in 1..=seed {
        index.add(document, vec![1.0, document as f32])?;
    }
    index.initialize()?;
    Ok(index)
}
fn left_changes(index: &mut KeyValueIVFIndex) -> StorageBackendResult<()> {
    index.add_many(11, vec![vec![1.0, 0.0], vec![0.0, 1.0]])?;
    index.delete(1)?;
    index.add(1, vec![0.25, 0.75])?;
    index.add_many(11, vec![])?;
    index.add_many(11, vec![vec![0.75, 0.25], vec![0.0, 1.0]])?;
    index.add(13, vec![0.5, 0.5])
}
fn right_changes(index: &mut KeyValueIVFIndex) -> StorageBackendResult<()> {
    index.add(12, vec![0.4, 0.6])?;
    index.delete(2)
}
fn compare(
    actual: &dyn KeyValueStore,
    expected: &dyn KeyValueStore,
    table: &str,
) -> StorageBackendResult<()> {
    for prefix in [
        vector_field_prefix(table, FIELD)?,
        ivf_metadata_key(table, FIELD)?,
        ivf_centroid_prefix(table, FIELD)?,
        ivf_assignment_prefix(table, FIELD)?,
    ] {
        expect_eq(
            &actual.scan_prefix(&prefix)?,
            &expected.scan_prefix(&prefix)?,
            "merged canonical vectors and complete IVF generation equal serial execution",
        )?;
    }
    Ok(())
}
fn serial(table: &str, seed: u64) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let mut index = create(store.clone(), table, seed)?;
    right_changes(&mut index)?;
    left_changes(&mut index)?;
    Ok(store)
}

/// Verify independent writers across initial training and retraining, ordered replacement/deletion and savepoint undo.
pub fn verify_ivf_document_merges(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    for seed in [0, 2, 8] {
        let table = format!("ivf-merge\0{seed}");
        let mut left = create(a.clone(), &table, seed)?;
        let mut right = KeyValueIVFIndex::restore(b.clone(), &table, FIELD, 2, params())?;
        let baseline = left.snapshot()?;
        a.begin_transaction()?;
        b.begin_transaction()?;
        a.savepoint("ivf_branch")?;
        left.add(99, vec![1.0, 0.0])?;
        let discarded = left.snapshot()?;
        a.rollback_to_savepoint("ivf_branch")?;
        a.release_savepoint("ivf_branch")?;
        left_changes(&mut left)?;
        let private = left.snapshot()?;
        right_changes(&mut right)?;
        b.commit_transaction()?;
        expect(
            a.in_transaction(),
            "second IVF writer commits before the first writer ends",
        )?;
        expect_eq(
            &left.count()?,
            &private.count()?,
            "private IVF view survives other writer publication",
        )?;
        a.commit_transaction()?;
        compare(a.as_ref(), serial(&table, seed)?.as_ref(), &table)?;
        expect_eq(
            &baseline.count()?,
            &(seed as usize),
            "old IVF generation remains pinned",
        )?;
        expect_eq(
            &discarded.count()?,
            &(seed as usize + 1),
            "discarded savepoint snapshot remains readable",
        )?;
        expect_eq(
            &left.count()?,
            &right.count()?,
            "both live IVF handles see the merged generation",
        )?;
    }
    Ok(())
}

/// Verify complete merged generations after all prior file handles close.
pub fn verify_ivf_merge_reopen(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    for seed in [0, 2, 8] {
        let table = format!("ivf-merge\0{seed}");
        compare(store.as_ref(), serial(&table, seed)?.as_ref(), &table)?;
        let index = KeyValueIVFIndex::restore(store.clone(), &table, FIELD, 2, params())?;
        let mut documents = index
            .search_knn(&[1.0, 0.0], 100)?
            .iter()
            .map(|posting| posting.doc_id)
            .collect::<Vec<_>>();
        documents.sort_unstable();
        let mut expected = (3..=seed).chain([1, 11, 12, 13]).collect::<Vec<_>>();
        expected.sort_unstable();
        expect_eq(&documents, &expected, "reopened merged IVF search")?;
    }
    Ok(())
}

/// Verify overlapping documents, rebuild/clear/drop and same-definition recreation retain their original conflicts.
pub fn verify_ivf_merge_conflicts(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    for case in 0..6 {
        for reverse in [false, true] {
            let table = format!("ivf-conflict-{case}-{reverse}");
            let mut left = create(a.clone(), &table, 3)?;
            let mut right = KeyValueIVFIndex::restore(b.clone(), &table, FIELD, 2, params())?;
            a.begin_transaction()?;
            b.begin_transaction()?;
            match case {
                0 => left.add_many(11, vec![])?,
                1 => left.delete(11)?,
                _ => left.add(11, vec![1.0, 0.0])?,
            }
            match case {
                0 | 1 => right.add(11, vec![0.0, 1.0])?,
                2 => right.clear()?,
                3 => right.initialize()?,
                _ => {
                    KeyValueIVFIndex::drop_metadata(b.as_ref(), &table, FIELD)?;
                    let definition = if case == 5 {
                        IVFIndexParams {
                            nlist: 3,
                            nprobe: 3,
                            train_threshold: 3,
                        }
                    } else {
                        params()
                    };
                    right = KeyValueIVFIndex::create(b.clone(), &table, FIELD, 2, definition)?;
                    right.initialize()?;
                }
            }
            let (winner, loser) = if reverse { (a, b) } else { (b, a) };
            winner.commit_transaction()?;
            let stable = winner.scan_prefix(b"")?;
            expect(
                loser.commit_transaction().is_err(),
                "overlapping or structural IVF change conflicts",
            )?;
            loser.rollback_transaction()?;
            expect_eq(
                &winner.scan_prefix(b"")?,
                &stable,
                "rejected IVF writer publishes no partial state",
            )?;
        }
    }
    verify_catalog_lifecycle(a, b)
}

fn verify_catalog_lifecycle(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    use crate::{CatalogFacade, KeyValueCatalog, RelationIdentity, TableSchema, VectorFieldSchema};
    let catalog = KeyValueCatalog::new(b.clone());
    catalog.save_schema("public")?;
    for case in 0..5 {
        for reverse in [false, true] {
            let table = format!("public.ivf_lifecycle_{case}_{reverse}");
            let destination = format!("{table}_renamed");
            let schema = TableSchema {
                relation: RelationIdentity::from_legacy_name(&table)
                    .expect("valid fixture relation"),
                role_owner: "uqa".into(),
                acl: None,
                column_acls: std::collections::BTreeMap::new(),
                object_id: [case + 1; 16],
                storage_generation: [case + 1; 16],
                analyzer_json: serde_json::to_string(&uqa_analysis::whitespace_analyzer())?,
                fts_fields: vec![],
                vector_fields: vec![VectorFieldSchema {
                    field: FIELD.into(),
                    dimensions: 2,
                }],
                columns_json: "[]".into(),
                constraints_json: String::new(),
            };
            catalog.save_table(&schema)?;
            let mut left = create(a.clone(), &table, 3)?;
            a.begin_transaction()?;
            b.begin_transaction()?;
            left.add(11, vec![0.5, 0.5])?;
            match case {
                0 => catalog.drop_table_and_data(&table)?,
                1 => catalog.purge_table_data(&table)?,
                2 => catalog.rename_table_data(&table, &destination)?,
                3 => catalog.drop_column_data(&table, FIELD)?,
                _ => catalog.rename_column_data(&table, FIELD, "replacement")?,
            }
            if case <= 1 {
                catalog.save_table(&schema)?;
                create(b.clone(), &table, 3)?;
            }
            let (winner, loser) = if reverse { (a, b) } else { (b, a) };
            winner.commit_transaction()?;
            let stable = winner.scan_prefix(b"")?;
            expect(
                loser.commit_transaction().is_err(),
                "IVF source and catalog lifecycle preserve conflicts in both orders",
            )?;
            loser.rollback_transaction()?;
            expect_eq(
                &winner.scan_prefix(b"")?,
                &stable,
                "IVF catalog conflict preserves the winner including recreated names",
            )?;
        }
    }
    Ok(())
}
