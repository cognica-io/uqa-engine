//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent document deltas share clusters and counters while structural and document conflicts remain strict.

use super::{expect, expect_eq};
use crate::{InvertedIndex, KeyValueInvertedIndex, KeyValueStore, StorageBackendResult};
use std::{collections::BTreeMap, sync::Arc};
use uqa_analysis::whitespace_analyzer;

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

pub(super) fn verify_reopen(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    for (case, (count, length)) in [(2, 5), (2, 9), (0, 0), (2, 7), (4, 8)]
        .into_iter()
        .enumerate()
    {
        let index = KeyValueInvertedIndex::new(
            store.clone(),
            format!("merge_occurrences_{case}"),
            whitespace_analyzer(),
        );
        expect_eq(
            &index.doc_count()?,
            &count,
            "reopened merged document count",
        )?;
        expect_eq(
            &index.total_field_length("body")?,
            &length,
            "reopened merged field length",
        )?;
        let documents: &[(u64, &str, u64)] = match case {
            0 => &[(1, "alpha", 2), (2, "alpha", 3)],
            1 => &[(1, "alpha", 4), (2, "alpha", 5)],
            2 => &[],
            3 => &[(2, "alpha", 3), (3, "alpha", 4)],
            _ => &[
                (1, "alpha", 2),
                (2, "alpha", 3),
                (65536, "gamma", 1),
                (u64::MAX, "delta", 2),
            ],
        };
        for &(document, term, frequency) in documents {
            expect_eq(
                &index.get_term_freq(document, "body", term)?,
                &frequency,
                "reopened merged posting frequency",
            )?;
            expect_eq(
                &index.get_doc_length(document, "body")?,
                &frequency,
                "reopened merged normalization length",
            )?;
        }
    }
    Ok(())
}

pub(super) fn verify(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    for case in 0..5 {
        let table = format!("merge_occurrences_{case}");
        let mut left = KeyValueInvertedIndex::new(a.clone(), &table, whitespace_analyzer());
        let mut right = KeyValueInvertedIndex::new(b.clone(), &table, whitespace_analyzer());
        if case != 0 {
            left.try_add_documents(vec![
                (1, fields("alpha alpha")),
                (2, fields("alpha alpha alpha")),
            ])?;
        }
        a.begin_transaction()?;
        a.savepoint("discarded")?;
        left.add_document(999, fields("discarded"))?;
        a.rollback_to_savepoint("discarded")?;
        let (count, length) = match case {
            0 => {
                left.add_document(1, fields("alpha alpha"))?;
                right.add_document(2, fields("alpha alpha alpha"))?;
                (2, 5)
            }
            1 => {
                left.add_document(1, fields("alpha alpha alpha alpha"))?;
                right.add_document(2, fields("alpha alpha alpha alpha alpha"))?;
                (2, 9)
            }
            2 => {
                left.remove_document(1)?;
                right.remove_document(2)?;
                (0, 0)
            }
            3 => {
                left.remove_document(1)?;
                right.add_document(3, fields("alpha alpha alpha alpha"))?;
                (2, 7)
            }
            _ => {
                left.add_document(65536, fields("gamma"))?;
                right.add_document(u64::MAX, fields("delta delta"))?;
                (4, 8)
            }
        };
        let retained = left.snapshot()?;
        let old_count = retained.doc_count()?;
        let old_length = retained.total_field_length("body")?;
        a.commit_transaction()?;
        for index in [&left, &right] {
            expect_eq(&index.doc_count()?, &count, "merged document count")?;
            expect_eq(
                &index.total_field_length("body")?,
                &length,
                "merged field length",
            )?;
            expect_eq(
                &index.get_doc_length(999, "body")?,
                &0,
                "rolled back changes are not merged",
            )?;
        }
        expect_eq(
            &retained.doc_count()?,
            &old_count,
            "retained private view does not advance after merge",
        )?;
        expect_eq(
            &retained.total_field_length("body")?,
            &old_length,
            "retained private counters do not advance after merge",
        )?;
    }
    verify_conflicts(a, b)?;
    verify_catalog_lifecycle(a, b)
}

fn verify_conflicts(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    for case in 0..6 {
        let table = format!("merge_occurrence_conflict_{case}");
        let mut left = KeyValueInvertedIndex::new(a.clone(), &table, whitespace_analyzer());
        let mut right = KeyValueInvertedIndex::new(b.clone(), &table, whitespace_analyzer());
        if case >= 3 {
            left.add_document(1, fields("alpha"))?;
        }
        a.begin_transaction()?;
        match case {
            0 => {
                left.add_document(1, fields("alpha"))?;
                right.add_document(1, BTreeMap::from([("another".into(), "beta".into())]))?;
            }
            1 | 3 => {
                left.add_document(2, fields("alpha"))?;
                right.clear()?;
            }
            2 | 4 => {
                left.clear()?;
                right.add_document(2, fields("alpha"))?;
            }
            _ => {
                left.add_document(2, fields("alpha"))?;
                right.try_rebuild_documents(vec![(1, fields("alpha"))])?;
            }
        }
        expect(
            a.commit_transaction().is_err(),
            "whole-document and structural conflicts must reject stale writers",
        )?;
        a.rollback_transaction()?;
        expect_eq(
            &left.doc_count()?,
            &right.doc_count()?,
            "conflict leaves the winner intact",
        )?;
    }
    Ok(())
}

fn verify_catalog_lifecycle(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    use crate::{CatalogFacade, KeyValueCatalog, RelationIdentity, TableSchema};
    let catalog = KeyValueCatalog::new(b.clone());
    catalog.save_schema("public")?;
    for case in 0..5 {
        let table = format!("public.occurrence_lifecycle_{case}");
        let destination = format!("{table}_renamed");
        catalog.save_table(&TableSchema {
            relation: RelationIdentity::from_legacy_name(&table).expect("valid test relation"),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: BTreeMap::new(),
            object_id: [case + 1; 16],
            storage_generation: [case + 1; 16],
            analyzer_json: serde_json::to_string(&whitespace_analyzer())?,
            fts_fields: vec!["body".into()],
            vector_fields: vec![],
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })?;
        let mut left = KeyValueInvertedIndex::new(a.clone(), &table, whitespace_analyzer());
        let mut right = KeyValueInvertedIndex::new(b.clone(), &table, whitespace_analyzer());
        right.add_document(1, fields("alpha"))?;
        a.begin_transaction()?;
        left.add_document(2, fields("alpha alpha"))?;
        match case {
            0 => catalog.drop_table_and_data(&table)?,
            1 => catalog.purge_table_data(&table)?,
            2 => catalog.rename_table_data(&table, &destination)?,
            3 => catalog.drop_column_data(&table, "body")?,
            _ => catalog.rename_column_data(&table, "body", "caption")?,
        }
        if case <= 1 {
            right.add_document(3, fields("alpha alpha alpha"))?;
        }
        expect(
            a.commit_transaction().is_err(),
            "stale source cannot cross catalog lifecycle or same-name recreation",
        )?;
        a.rollback_transaction()?;
        let target = KeyValueInvertedIndex::new(
            b.clone(),
            if case == 2 { &destination } else { &table },
            whitespace_analyzer(),
        );
        expect_eq(
            &target.get_doc_length(2, "body")?,
            &0,
            "stale source was not published",
        )?;
        if case <= 1 {
            expect_eq(
                &target.get_doc_length(3, "body")?,
                &3,
                "replacement namespace survives stale writer",
            )?;
        }
    }
    Ok(())
}
