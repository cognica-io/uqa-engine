//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind physical filter leaves to the shared query index reconciliation.

use super::context::RetrievalRelations;
use uqa_core::{DocId, Payload, PostingEntry, PostingList, Predicate};
use uqa_sql::SQLError;

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
                return Err(SQLError::Internal(format!(
                    "Filter consistency error: candidate {doc_id} is missing from the document-field snapshot for table `{table}`"
                )));
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
