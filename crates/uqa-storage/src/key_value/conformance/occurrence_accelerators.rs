//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted bounds and skip positions share source history and conflict admission.

use super::{expect, expect_eq};
use crate::{
    block_max_index::BlockMaxScorer, InvertedIndex, KeyValueInvertedIndex, KeyValueStore,
    StorageBackendResult,
};
use std::{cell::Cell, collections::BTreeMap, sync::Arc};
use uqa_analysis::whitespace_analyzer;

struct Frequency;
impl BlockMaxScorer for Frequency {
    fn score(&self, frequency: u64, _: u64, _: u64) -> f64 {
        frequency as f64
    }
}
struct InvalidLate(Cell<usize>);
impl BlockMaxScorer for InvalidLate {
    fn score(&self, _: u64, _: u64, _: u64) -> f64 {
        let count = self.0.get() + 1;
        self.0.set(count);
        if count == 129 {
            f64::NAN
        } else {
            9.0
        }
    }
}
fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

/// Verify bounds, source invalidation, retained branches and late competing builds. Requires two disposable sessions of one store.
pub fn verify_occurrence_accelerators(
    a: &Arc<dyn KeyValueStore>,
    b: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let mut index = KeyValueInvertedIndex::new(a.clone(), "accelerators", whitespace_analyzer());
    let mut other = KeyValueInvertedIndex::new(b.clone(), "accelerators", whitespace_analyzer());
    index.try_add_documents(
        (0..129)
            .map(|id| {
                (
                    id * 3,
                    fields(if id == 128 {
                        "alpha alpha alpha"
                    } else {
                        "alpha alpha"
                    }),
                )
            })
            .collect(),
    )?;
    index.flush_skip_pointers()?;
    expect_eq(
        &index.skip_to("body", "alpha", 384)?,
        &(384, 128),
        "skip positions count postings rather than document ids",
    )?;
    index.rebuild_persisted_block_max("body", &Frequency, "frequency")?;
    let expected = Some(vec![2.0, 3.0]);
    expect_eq(
        &index.persisted_block_max_scores("body", "alpha", "frequency")?,
        &expected,
        "bounds span the last partial block",
    )?;
    expect_eq(
        &index.persisted_block_max_scores("body", "alpha", "another")?,
        &None,
        "scorer identity is required",
    )?;
    verify_failed_build(&mut index)?;
    let retained = index.snapshot()?;
    a.begin_transaction()?;
    a.savepoint("bounds")?;
    index.add_document(0, fields("alpha alpha alpha alpha"))?;
    expect_eq(
        &index.persisted_block_max_scores("body", "alpha", "frequency")?,
        &None,
        "source mutation invalidates bounds",
    )?;
    expect_eq(
        &index.skip_to("body", "alpha", 384)?,
        &(0, 0),
        "source mutation invalidates skips",
    )?;
    index.rebuild_persisted_block_max("body", &Frequency, "private")?;
    let private = index.snapshot()?;
    a.rollback_to_savepoint("bounds")?;
    a.commit_transaction()?;
    expect_eq(
        &index.persisted_block_max_scores("body", "alpha", "frequency")?,
        &expected,
        "savepoint restores bounds and source",
    )?;
    expect_eq(
        &private.persisted_block_max_scores("body", "alpha", "private")?,
        &Some(vec![4.0, 3.0]),
        "discarded private bounds remain paired with source",
    )?;
    index.add_document(0, fields("alpha"))?;
    a.begin_transaction()?;
    index.add_document(0, fields("alpha alpha alpha alpha"))?;
    other.rebuild_persisted_block_max("body", &Frequency, "late")?;
    a.commit_transaction()?;
    expect_eq(
        &other.persisted_block_max_scores("body", "alpha", "late")?,
        &None,
        "source merge invalidates a later cache build",
    )?;
    a.begin_transaction()?;
    index.rebuild_persisted_block_max("body", &Frequency, "stale")?;
    other.add_document(0, fields("alpha"))?;
    expect(
        a.commit_transaction().is_err(),
        "stale scorer output cannot publish after a source change",
    )?;
    a.rollback_transaction()?;
    index.clear()?;
    expect_eq(
        &index.persisted_block_max_scores("body", "alpha", "late")?,
        &None,
        "clear retires all accelerators",
    )?;
    expect_eq(
        &retained.persisted_block_max_scores("body", "alpha", "frequency")?,
        &expected,
        "committed retained bounds survive clear",
    )?;
    Ok(())
}

fn verify_failed_build(index: &mut KeyValueInvertedIndex) -> StorageBackendResult<()> {
    let invalid = InvalidLate(Cell::new(0));
    expect(
        index
            .rebuild_persisted_block_max("body", &invalid, "invalid")
            .is_err(),
        "late invalid scores discard the complete build",
    )?;
    expect_eq(
        &invalid.0.get(),
        &129,
        "scorer is evaluated once per source posting",
    )?;
    expect_eq(
        &index.persisted_block_max_scores("body", "alpha", "frequency")?,
        &Some(vec![2.0, 3.0]),
        "failed builds retain previous bounds",
    )
}
