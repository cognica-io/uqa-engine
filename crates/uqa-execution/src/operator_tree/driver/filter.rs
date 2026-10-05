//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconcile value-index candidates with the selected command and transaction changes.

use crate::query::document_changes::DocumentChanges;
use uqa_core::{CancellationToken, Payload, PostingEntry, PostingList, Predicate};
use uqa_sql::SQLError;
use uqa_storage::DocumentStore;

impl super::PhysicalRetrievalDriver<'_> {
    pub(super) fn value_index_scan(
        &self,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError> {
        self.context
            .indexes
            .value_index_scan(self.table, field, predicate)?
            .map(|indexed| {
                merge_changes(
                    indexed,
                    self.context.relations.command_overlay_changes(self.table)?,
                    field,
                    predicate,
                    &self.context.runtime.cancellation_token(),
                )
            })
            .transpose()
    }
}

fn merge_changes(
    indexed: PostingList,
    changes: Option<DocumentChanges>,
    field: &str,
    predicate: &Predicate,
    cancellation: &CancellationToken,
) -> Result<PostingList, SQLError> {
    let Some(changes) = changes.filter(DocumentChanges::has_changes) else {
        return Ok(indexed);
    };
    let error =
        |error| crate::storage_errors::storage_error("read indexed command changes", &error);
    let mut candidates = indexed.entries().iter().peekable();
    let mut entries = Vec::with_capacity(indexed.len());
    for change in changes.changes() {
        cancellation.check()?;
        let (id, present) = change.map_err(error)?;
        while candidates.peek().is_some_and(|entry| entry.doc_id < id) {
            entries.push(candidates.next().expect("a peeked candidate").clone());
        }
        if candidates.peek().is_some_and(|entry| entry.doc_id == id) {
            candidates.next();
        }
        // Only the changed field is read. Its newest value or tombstone replaces the stored posting, including an identity absent from the stored index.
        if present && predicate.evaluate(changes.get_field(id, field).map_err(error)?.as_ref()) {
            entries.push(PostingEntry::new(id, Payload::default()));
        }
    }
    entries.extend(candidates.cloned());
    Ok(PostingList::from_sorted_unchecked(entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::overlay::CommandMutationOverlay;
    use std::{collections::BTreeMap, sync::Arc};
    use uqa_core::{memory::MemoryBudget, Value};
    use uqa_storage::{read_control::StorageReadControl, DocumentMetadata};

    #[test]
    fn indexed_filters_merge_newest_rows_tombstones_and_nulls() {
        let memory = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let control = StorageReadControl::new(&memory, &cancellation);
        let mut overlays = [
            CommandMutationOverlay::default(),
            CommandMutationOverlay::default(),
        ];
        for (frame, id, value) in [
            (0, 1, Some(Value::Int(10))),
            (0, 2, Some(Value::Int(10))),
            (0, 4, Some(Value::Int(10))),
            (0, 5, Some(Value::Int(10))),
            (1, 1, Some(Value::Int(20))),
            (1, 2, None),
            (1, 4, Some(Value::Null)),
        ] {
            overlays[frame]
                .stage(
                    "t",
                    id,
                    value.map(|value| {
                        (
                            Arc::new(BTreeMap::from([("v".into(), value)])),
                            DocumentMetadata::default(),
                        )
                    }),
                    &control,
                )
                .unwrap();
        }
        let changes =
            CommandMutationOverlay::changes(&overlays, "t", DocumentChanges::default(), &control)
                .unwrap();
        // Stored identity 3 survives without a field read; 1, 2 and 4 are masked. Identity 5 exists only in the command.
        for (predicate, stored, expected) in [
            (
                Predicate::Equals(Value::Int(10)),
                vec![1, 2, 3, 4],
                vec![3, 5],
            ),
            (
                Predicate::GreaterThan(Value::Int(15)),
                vec![2, 3],
                vec![1, 3],
            ),
            (Predicate::IsNull, vec![2, 3], vec![3, 4]),
        ] {
            let indexed = PostingList::from_sorted_unchecked(
                stored
                    .into_iter()
                    .map(|id| PostingEntry::new(id, Payload::default()))
                    .collect(),
            );
            let result = merge_changes(
                indexed,
                Some(changes.clone()),
                "v",
                &predicate,
                &cancellation,
            )
            .unwrap();
            assert_eq!(
                result
                    .entries()
                    .iter()
                    .map(|entry| entry.doc_id)
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
}
