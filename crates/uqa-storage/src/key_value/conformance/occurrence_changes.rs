//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated text changes distinguish term intent, document metadata and scoring scalars.

use std::collections::{BTreeMap, BTreeSet};

use super::{expect, expect_eq};
use crate::inverted_index::InvertedIndexChange;
use crate::{InvertedIndex, StorageBackendError, StorageBackendResult, TokenTermKey};

#[derive(Debug, Default, PartialEq, Eq)]
struct Changes {
    postings: BTreeSet<(u64, String, TokenTermKey)>,
    documents: BTreeSet<(u64, String)>,
    statistics: BTreeSet<String>,
}

impl Changes {
    fn visit(&mut self, change: InvertedIndexChange<'_>) {
        match change {
            InvertedIndexChange::Posting {
                doc_id,
                field,
                term,
            } => {
                self.postings.insert((doc_id, field.into(), term.clone()));
            }
            InvertedIndexChange::Document { doc_id, field } => {
                self.documents.insert((doc_id, field.into()));
            }
            InvertedIndexChange::FieldStatistics { field } => {
                self.statistics.insert(field.into());
            }
        }
    }
}

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("body".into(), text.into()),
        ("empty".into(), String::new()),
    ])
}

/// Verify evaluated change capture against a disposable, empty index using whitespace analysis. Includes same-cluster isolation, absent versus zero-token fields, and atomic rejection with typed resource errors.
pub fn verify_inverted_index_changes(index: &mut dyn InvertedIndex) -> StorageBackendResult<()> {
    index.add_document(2, fields("shared untouched"))?;
    let mut added = Changes::default();
    index.try_add_documents_observed(vec![(1, fields("alpha alpha"))], &mut |change| {
        added.visit(change);
        Ok(())
    })?;
    let alpha = TokenTermKey::from_text("alpha");
    let beta = TokenTermKey::from_text("beta");
    expect_eq(
        &added,
        &Changes {
            postings: BTreeSet::from([(1, "body".into(), alpha.clone())]),
            documents: BTreeSet::from([(1, "body".into()), (1, "empty".into())]),
            statistics: BTreeSet::from(["body".into(), "empty".into()]),
        },
        "new field membership observes statistics even when analysis emits no tokens",
    )?;
    let mut replaced = Changes::default();
    index.try_add_documents_observed(vec![(1, fields("beta beta"))], &mut |change| {
        replaced.visit(change);
        Ok(())
    })?;
    expect_eq(
        &replaced,
        &Changes {
            postings: BTreeSet::from([(1, "body".into(), alpha), (1, "body".into(), beta.clone())]),
            documents: added.documents.clone(),
            statistics: BTreeSet::new(),
        },
        "same-length replacement retains old and new terms without a physical-counter conflict",
    )?;
    expect_eq(
        &index.doc_freq("body", "shared")?,
        &1,
        "shared cluster retains unrelated postings",
    )?;
    verify_capture_failure(index)?;

    let mut missing = Changes::default();
    index.try_remove_document_observed(99, &mut |change| {
        missing.visit(change);
        Ok(())
    })?;
    expect_eq(
        &missing,
        &Changes::default(),
        "absent deletion has no logical text intent",
    )?;
    let mut removed = Changes::default();
    index.try_remove_document_observed(1, &mut |change| {
        removed.visit(change);
        Ok(())
    })?;
    expect_eq(
        &removed,
        &Changes {
            postings: BTreeSet::from([(1, "body".into(), beta)]),
            documents: added.documents,
            statistics: added.statistics,
        },
        "deletion uses persisted original terms and removes zero-token membership",
    )?;
    expect_eq(
        &index.doc_count()?,
        &1,
        "observed deletion leaves the other document",
    )?;
    expect_eq(
        &index.total_field_length("body")?,
        &2,
        "observed deletion preserves unrelated length",
    )?;
    Ok(())
}

fn verify_capture_failure(index: &mut dyn InvertedIndex) -> StorageBackendResult<()> {
    for cancelled in [false, true] {
        let error = index
            .try_add_documents_observed(
                vec![(1, fields("rejected longer text")), (3, fields("new"))],
                &mut |change| {
                    if matches!(change, InvertedIndexChange::Document { doc_id: 3, .. }) {
                        return Err(if cancelled {
                            StorageBackendError::Cancelled(uqa_core::QueryCancelled)
                        } else {
                            uqa_core::memory::MemoryError::SizeOverflow.into()
                        });
                    }
                    Ok(())
                },
            )
            .expect_err("the capturing visitor rejects the later document");
        expect(
            matches!(
                (&error, cancelled),
                (StorageBackendError::Cancelled(_), true) | (StorageBackendError::Memory(_), false)
            ),
            "capture failure preserves its typed resource error",
        )?;
        expect_eq(
            &index.get_term_freq(1, "body", "beta")?,
            &2,
            "capture failure preserves original occurrences",
        )?;
        expect_eq(
            &index.doc_freq("body", "rejected")?,
            &0,
            "capture failure publishes no replacement",
        )?;
        expect(
            index.indexed_field_metadata(3, "body")?.is_none(),
            "capture failure publishes no later document",
        )?;
        expect_eq(
            &index.total_field_length("body")?,
            &4,
            "capture failure preserves field statistics",
        )?;
    }

    Ok(())
}
