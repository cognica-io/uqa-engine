//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Concurrent vector generations agree byte-for-byte with the serial index owner.

use std::sync::Arc;

use super::{expect, expect_eq};
use crate::{
    key_value::{
        codec::vector_field_prefix,
        index_keys::{
            hnsw_metadata_key, hnsw_node_prefix, ivf_assignment_prefix, ivf_centroid_prefix,
            ivf_metadata_key,
        },
        KeyValueHNSWIndex, KeyValueIVFIndex,
    },
    HNSWIndexParams, IVFIndexParams, KeyValueStore, MemoryKeyValueStore, StorageBackendResult,
    VectorIndex,
};

const FIELD: &str = "embedding\0日本語";
#[derive(Clone, Copy, Debug)]
pub enum VectorMergeKind {
    IVF,
    HNSW,
}
impl VectorMergeKind {
    pub(super) fn open(
        self,
        store: Arc<dyn KeyValueStore>,
        table: &str,
        restore: bool,
        alternate: bool,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(match self {
            Self::IVF => {
                let params = IVFIndexParams {
                    nlist: if alternate { 3 } else { 2 },
                    nprobe: 2,
                    train_threshold: 3,
                };
                Box::new(if restore {
                    KeyValueIVFIndex::restore(store, table, FIELD, 2, params)?
                } else {
                    KeyValueIVFIndex::create(store, table, FIELD, 2, params)?
                })
            }
            Self::HNSW => {
                let params = HNSWIndexParams {
                    m: if alternate { 3 } else { 2 },
                    ef_construction: 8,
                    ef_search: 64,
                    rebuild_threshold: 1,
                    seed: 7,
                };
                Box::new(if restore {
                    KeyValueHNSWIndex::restore(store, table, FIELD, 2, params)?
                } else {
                    KeyValueHNSWIndex::create(store, table, FIELD, 2, params)?
                })
            }
        })
    }
}
fn create(
    store: Arc<dyn KeyValueStore>,
    table: &str,
    seed: u64,
    kind: VectorMergeKind,
) -> StorageBackendResult<Box<dyn VectorIndex>> {
    let mut index = kind.open(store, table, false, false)?;
    for document in 1..=seed {
        index.add(document, vec![1.0, document as f32])?;
    }
    index.initialize()?;
    Ok(index)
}
fn left_changes(index: &mut dyn VectorIndex) -> StorageBackendResult<()> {
    index.add_many(11, vec![vec![1.0, 0.0], vec![0.0, 1.0]])?;
    index.delete(1)?;
    index.add(1, vec![0.25, 0.75])?;
    index.add_many(11, vec![])?;
    index.add_many(11, vec![vec![0.75, 0.25], vec![0.0, 1.0]])?;
    index.add(13, vec![0.5, 0.5])
}
fn right_changes(index: &mut dyn VectorIndex) -> StorageBackendResult<()> {
    index.add(12, vec![0.4, 0.6])?;
    index.delete(2)
}
fn compare(
    actual: &dyn KeyValueStore,
    expected: &dyn KeyValueStore,
    table: &str,
    kind: VectorMergeKind,
) -> StorageBackendResult<()> {
    let derived = match kind {
        VectorMergeKind::IVF => vec![
            ivf_metadata_key(table, FIELD)?,
            ivf_centroid_prefix(table, FIELD)?,
            ivf_assignment_prefix(table, FIELD)?,
        ],
        VectorMergeKind::HNSW => vec![
            hnsw_metadata_key(table, FIELD)?,
            hnsw_node_prefix(table, FIELD)?,
        ],
    };
    for prefix in std::iter::once(vector_field_prefix(table, FIELD)?).chain(derived) {
        expect_eq(
            &actual.scan_prefix(&prefix)?,
            &expected.scan_prefix(&prefix)?,
            "merged canonical vectors and complete vector generation equal serial execution",
        )?;
    }
    Ok(())
}
fn serial(
    table: &str,
    seed: u64,
    kind: VectorMergeKind,
) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
    let store: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    let mut index = create(store.clone(), table, seed, kind)?;
    right_changes(&mut *index)?;
    left_changes(&mut *index)?;
    Ok(store)
}

/// Verify independent writers across initial training and retraining, ordered replacement/deletion and savepoint undo.
pub fn verify_vector_document_merges(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
    kind: VectorMergeKind,
) -> StorageBackendResult<()> {
    for seed in [0, 2, 8] {
        let table = format!("{kind:?}-merge\0{seed}");
        let mut left = create(a.clone(), &table, seed, kind)?;
        let mut right = kind.open(b.clone(), &table, true, false)?;
        let baseline = left.snapshot()?;
        a.begin_transaction()?;
        b.begin_transaction()?;
        a.savepoint("vector_branch")?;
        left.add(99, vec![1.0, 0.0])?;
        let discarded = left.snapshot()?;
        a.rollback_to_savepoint("vector_branch")?;
        a.release_savepoint("vector_branch")?;
        left_changes(&mut *left)?;
        let private = left.snapshot()?;
        right_changes(&mut *right)?;
        b.commit_transaction()?;
        expect(
            a.in_transaction(),
            "second vector writer commits before the first writer ends",
        )?;
        expect_eq(
            &left.count()?,
            &private.count()?,
            "private vector view survives other writer publication",
        )?;
        a.commit_transaction()?;
        compare(
            a.as_ref(),
            serial(&table, seed, kind)?.as_ref(),
            &table,
            kind,
        )?;
        expect_eq(
            &baseline.count()?,
            &(seed as usize),
            "old vector generation remains pinned",
        )?;
        expect_eq(
            &discarded.count()?,
            &(seed as usize + 1),
            "discarded savepoint snapshot remains readable",
        )?;
        expect_eq(
            &left.count()?,
            &right.count()?,
            "both live vector handles see the merged generation",
        )?;
    }
    Ok(())
}

/// Verify complete merged generations after all prior file handles close.
pub fn verify_vector_merge_reopen(
    store: &Arc<dyn KeyValueStore>,
    kind: VectorMergeKind,
) -> StorageBackendResult<()> {
    for seed in [0, 2, 8] {
        let table = format!("{kind:?}-merge\0{seed}");
        compare(
            store.as_ref(),
            serial(&table, seed, kind)?.as_ref(),
            &table,
            kind,
        )?;
        let index = kind.open(store.clone(), &table, true, false)?;
        let mut documents = index
            .search_knn(&[1.0, 0.0], 100)?
            .iter()
            .map(|posting| posting.doc_id)
            .collect::<Vec<_>>();
        documents.sort_unstable();
        let mut expected = (3..=seed).chain([1, 11, 12, 13]).collect::<Vec<_>>();
        expected.sort_unstable();
        expect_eq(&documents, &expected, "reopened merged vector search")?;
    }
    Ok(())
}

/// Verify overlapping documents, rebuild/clear/drop and same-definition recreation retain their original conflicts.
pub fn verify_vector_merge_conflicts(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
    kind: VectorMergeKind,
) -> StorageBackendResult<()> {
    for case in 0..6 {
        for reverse in [false, true] {
            let table = format!("{kind:?}-conflict-{case}-{reverse}");
            let mut left = create(a.clone(), &table, 3, kind)?;
            let mut right = kind.open(b.clone(), &table, true, false)?;
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
                    right = kind.open(b.clone(), &table, false, case == 5)?;
                    right.initialize()?;
                }
            }
            let (winner, loser) = if reverse { (a, b) } else { (b, a) };
            winner.commit_transaction()?;
            let stable = winner.scan_prefix(b"")?;
            expect(
                loser.commit_transaction().is_err(),
                "overlapping or structural vector change conflicts",
            )?;
            loser.rollback_transaction()?;
            expect_eq(
                &winner.scan_prefix(b"")?,
                &stable,
                "rejected vector writer publishes no partial state",
            )?;
        }
    }
    verify_catalog_lifecycle(a, b, kind)
}

fn verify_catalog_lifecycle(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
    kind: VectorMergeKind,
) -> StorageBackendResult<()> {
    use crate::{CatalogFacade, KeyValueCatalog, RelationIdentity, TableSchema, VectorFieldSchema};
    let catalog = KeyValueCatalog::new(b.clone());
    catalog.save_schema("public")?;
    for case in 0..5 {
        for reverse in [false, true] {
            let table = format!("public.{kind:?}_lifecycle_{case}_{reverse}");
            let destination = format!("{table}_renamed");
            let schema = TableSchema {
                relation: RelationIdentity::from_legacy_name(&table)
                    .expect("valid fixture relation"),
                security: crate::RelationSecurityRow::legacy("uqa"),
                object_id: [1
                    + case * 4
                    + u8::from(reverse) * 2
                    + u8::from(matches!(kind, VectorMergeKind::HNSW));
                    16],
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
            let mut left = create(a.clone(), &table, 3, kind)?;
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
                create(b.clone(), &table, 3, kind)?;
            }
            let (winner, loser) = if reverse { (a, b) } else { (b, a) };
            winner.commit_transaction()?;
            let stable = winner.scan_prefix(b"")?;
            expect(
                loser.commit_transaction().is_err(),
                "vector source and catalog lifecycle preserve conflicts in both orders",
            )?;
            loser.rollback_transaction()?;
            expect_eq(
                &winner.scan_prefix(b"")?,
                &stable,
                "vector catalog conflict preserves the winner including recreated names",
            )?;
        }
    }
    Ok(())
}
