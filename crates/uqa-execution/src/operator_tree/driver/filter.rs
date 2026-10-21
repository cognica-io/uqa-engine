//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind physical filter leaves to the shared query index reconciliation.

use super::context::RetrievalRelations;
use uqa_core::{
    CancellationToken, DocId, Payload, PostingEntry, PostingList, Predicate, ScoredEntry,
};
use uqa_sql::SQLError;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Candidates<'a> {
    /// The filter's source promises documents in the selected relation.
    Documents(&'a [DocId]),
    /// Other intersection operands can carry graph identities outside that relation.
    Intersection(&'a [DocId]),
}

pub(super) fn evaluate(
    relations: &dyn RetrievalRelations,
    table: &str,
    field: &str,
    predicate: &Predicate,
    candidates: Candidates<'_>,
    check_cancelled: impl Fn() -> Result<(), SQLError>,
) -> Result<PostingList, SQLError> {
    check_cancelled()?;
    let (ids, require_documents) = match candidates {
        Candidates::Documents(ids) => (ids, true),
        Candidates::Intersection(ids) => (ids, false),
    };
    let values = relations.get_document_fields(table, ids, field)?;
    let mut entries = Vec::with_capacity(ids.len());
    for &doc_id in ids {
        check_cancelled()?;
        let Some(value) = values.get(&doc_id) else {
            if require_documents {
                return Err(missing_document(table, doc_id));
            }
            // A standalone relational filter has no posting for an absent row.
            // Restricting its work to another operand's support must preserve that absence.
            continue;
        };
        if predicate.evaluate(Some(value)) {
            entries.push(PostingEntry::new(doc_id, Payload::default()));
        }
    }
    entries.sort_by_key(|entry| entry.doc_id);
    Ok(PostingList::from_sorted_unchecked(entries))
}

pub(super) fn capture(
    relations: &(impl RetrievalRelations + ?Sized),
    table: &str,
    field: &str,
    predicate: &Predicate,
    candidates: &crate::query::SelectedIds,
    bytes: usize,
    cancellation: &CancellationToken,
) -> Result<crate::query::scored_input::CapturedScoredEntries, SQLError> {
    let mut output =
        crate::query::scored_input::ScoredEntryCapture::new(bytes, cancellation.clone())?;
    let mut missing = None;
    for id in candidates.cursor() {
        cancellation.check()?;
        uqa_sql::expr::check_statement_cancellation()?;
        let id = id?;
        // A projected value can be arbitrarily wide. Retain one value instead of a row-count-only batch or the complete population.
        let values = relations.get_document_fields(table, &[id], field)?;
        match values.get(&id) {
            Some(value) if missing.is_none() && predicate.evaluate(Some(value)) => {
                output.push(ScoredEntry {
                    doc_id: id,
                    score: 0.0,
                })?;
            }
            Some(_) => {}
            None => {
                missing.get_or_insert(id);
            }
        }
    }
    // The ordinary filter reads all projected fields before diagnosing a missing promised row. A later source error must still take precedence.
    if let Some(id) = missing {
        return Err(missing_document(table, id));
    }
    output.finish()
}

fn missing_document(table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "Filter consistency error: candidate {doc_id} is missing from the document-field snapshot for table `{table}`"
    ))
}

impl super::PhysicalRetrievalDriver<'_> {
    pub(super) fn value_index_scan(
        &self,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        crate::query::indexed_filter::scan(
            self.context.indexes,
            self.table,
            field,
            predicate,
            &self.context.runtime.cancellation_token(),
        )
    }
}

#[cfg(test)]
mod tests;
