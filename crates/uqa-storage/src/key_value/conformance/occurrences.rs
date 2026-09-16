//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Occurrence data, statistics and analyzer metadata retain one transaction boundary.

use super::{expect, expect_eq};
use crate::clustered_postings::PostingReadCursor;
use crate::{
    AnalyzerPhase, InvertedIndex, KeyValueInvertedIndex, KeyValueStore, StorageBackendResult,
    TokenTermKey,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_analysis::whitespace_analyzer;

const TABLE: &str = "occurrences\0日本語";
fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

/// Verify retained committed/private occurrence snapshots, savepoint branches and mutation rejection. Requires a disposable store.
pub fn verify_occurrence_snapshots(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let mut index = KeyValueInvertedIndex::new(store.clone(), TABLE, whitespace_analyzer());
    index.add_document(1, fields("alpha alpha beta"))?;
    let baseline = index.snapshot()?;
    let nested = baseline.snapshot()?;
    store.begin_transaction()?;
    index.add_document(65536, fields("alpha beta"))?;
    let private = index.snapshot()?;
    expect_eq(
        &baseline.doc_count()?,
        &1,
        "occurrence snapshot ignores private writes",
    )?;
    store.rollback_transaction()?;
    expect_eq(
        &index.doc_count()?,
        &1,
        "live occurrence handle follows rollback",
    )?;
    expect_eq(
        &private.doc_count()?,
        &2,
        "private occurrence snapshot survives rollback",
    )?;
    expect_eq(
        &private.total_field_length("body")?,
        &5,
        "private occurrence statistics remain paired",
    )?;

    store.begin_transaction()?;
    store.savepoint("occurrences")?;
    index.remove_document(1)?;
    index.add_document(u64::MAX, fields("discarded"))?;
    let discarded = index.snapshot()?;
    store.rollback_to_savepoint("occurrences")?;
    index.add_document(2, fields("committed"))?;
    store.commit_transaction()?;
    expect_eq(
        &discarded.get_total_doc_length(u64::MAX)?,
        &1,
        "discarded occurrence branch remains readable",
    )?;
    expect_eq(
        &discarded.doc_freq("body", "alpha")?,
        &0,
        "discarded branch preserves removals",
    )?;
    expect_eq(
        &index.doc_count()?,
        &2,
        "live occurrence handle follows commit",
    )?;
    drop(index);

    verify_retained_occurrences(baseline, nested.as_ref())
}

fn verify_retained_occurrences(
    mut baseline: Arc<dyn InvertedIndex>,
    nested: &dyn InvertedIndex,
) -> StorageBackendResult<()> {
    let control = crate::read_control::StorageReadControl::with_limit(1 << 20);
    let term = TokenTermKey::from_text("alpha");
    for snapshot in [baseline.as_ref(), nested] {
        expect_eq(
            &snapshot.field_stats("body")?.total_docs,
            &1,
            "retained field statistics",
        )?;
        expect_eq(
            &snapshot
                .field_stats_scalar_budgeted("body", &control)?
                .avg_doc_length,
            &3.0,
            "retained scalar statistics",
        )?;
        expect_eq(
            &snapshot
                .get_occurrences_budgeted(1, "body", &term, &control)?
                .len(),
            &2,
            "retained complete occurrences",
        )?;
        expect_eq(
            &snapshot.get_scoring_inputs_keys_bulk(
                &[1, 1],
                "body",
                &[term.clone(), term.clone()],
            )?,
            &vec![(3, vec![2, 2]); 2],
            "retained bulk scoring inputs preserve repeated ids and terms",
        )?;
        let mut cursor = snapshot.posting_read_cursor_key_budgeted("body", &term, &control)?;
        expect_eq(
            &cursor.current().map(|entry| entry.doc_id),
            &Some(1),
            "retained controlled cursor",
        )?;
        expect(
            cursor.advance()?.is_none(),
            "retained cursor excludes later clusters",
        )?;
    }
    let writable = Arc::get_mut(&mut baseline).expect("snapshot handle is uniquely owned");
    expect(
        writable.add_document(9, fields("forbidden")).is_err(),
        "snapshot rejects insertion",
    )?;
    expect(
        writable.remove_document(1).is_err(),
        "snapshot rejects removal",
    )?;
    expect(writable.clear().is_err(), "snapshot rejects clearing")?;
    expect(
        writable
            .try_rebuild_documents(vec![(9, fields("forbidden"))])
            .is_err(),
        "snapshot rejects rebuild",
    )?;
    expect(
        writable
            .set_field_analyzer("body", whitespace_analyzer(), AnalyzerPhase::Both)
            .is_err(),
        "snapshot rejects analyzer replacement",
    )?;
    expect_eq(
        &writable.doc_count()?,
        &1,
        "rejected snapshot mutations preserve retained rows",
    )?;
    expect_eq(
        &nested.doc_count()?,
        &1,
        "nested snapshot shares immutable rows",
    )?;
    Ok(())
}

/// Verify independent index writers and an explicitly pinned reader. This does not claim merging concurrent updates of one shared posting cluster or field counter.
pub fn verify_occurrence_concurrency(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let mut left = KeyValueInvertedIndex::new(a.clone(), "occurrence_left", whitespace_analyzer());
    let mut right =
        KeyValueInvertedIndex::new(b.clone(), "occurrence_right", whitespace_analyzer());
    left.add_document(1, fields("alpha"))?;
    right.add_document(1, fields("beta"))?;
    a.begin_transaction()?;
    left.add_document(2, fields("alpha alpha"))?;
    b.begin_transaction()?;
    right.add_document(2, fields("beta beta"))?;
    b.commit_transaction()?;
    expect(
        a.in_transaction(),
        "other occurrence writer commits before first finishes",
    )?;
    a.commit_transaction()?;
    expect_eq(
        &right.total_field_length("body")?,
        &3,
        "independent field counters survive",
    )?;
    a.begin_read_transaction()?;
    let retained = left.snapshot()?;
    let mut external =
        KeyValueInvertedIndex::new(b.clone(), "occurrence_left", whitespace_analyzer());
    external.add_document(3, fields("new"))?;
    expect_eq(
        &left.doc_count()?,
        &2,
        "occurrence reads retain explicit transaction boundary",
    )?;
    a.rollback_transaction()?;
    expect_eq(
        &left.doc_count()?,
        &3,
        "later occurrence read observes sibling commit",
    )?;
    expect_eq(
        &retained.doc_count()?,
        &2,
        "retained occurrence snapshot outlives pinned reader",
    )?;
    Ok(())
}

/// Verify the durable results of `verify_occurrence_concurrency` after all original sessions close.
pub fn verify_occurrence_reopen(store: Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let left = KeyValueInvertedIndex::new(store.clone(), "occurrence_left", whitespace_analyzer());
    let right = KeyValueInvertedIndex::new(store, "occurrence_right", whitespace_analyzer());
    expect_eq(&left.doc_count()?, &3, "reopened occurrence documents")?;
    expect_eq(
        &left.total_field_length("body")?,
        &4,
        "reopened occurrence counters",
    )?;
    expect_eq(
        &left.doc_freq("body", "alpha")?,
        &2,
        "reopened occurrence support",
    )?;
    expect_eq(
        &right.doc_count()?,
        &2,
        "reopened independent occurrence documents",
    )?;
    Ok(())
}
