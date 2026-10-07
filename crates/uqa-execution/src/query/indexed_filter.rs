//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconcile selected storage indexes with cached command keys and retained private versions.

use super::document_changes::DocumentChanges;
use crate::mutation::overlay::CommandIndexProbe;
use crate::storage_errors::storage_error;
use uqa_core::{CancellationToken, Payload, PostingEntry, PostingList, Predicate, Value};
use uqa_sql::SQLError;
use uqa_storage::DocumentStore;

/// All inputs refer to the query's selected relation generation and original read allowance.
pub trait QueryIndexRead: Sync {
    /// Probe stored values and record the original serializable predicate observation.
    fn value_index_scan(
        &self,
        table: &str,
        field: &str,
        predicate: &Predicate,
    ) -> Result<Option<PostingList>, SQLError>;
    fn command_overlay_changes(&self, table: &str) -> Result<Option<DocumentChanges>, SQLError>;
    /// Capture command-only masks together with cached matches from the same frame stack.
    fn exact_command_matches(
        &self,
        table: &str,
        field: &str,
        value: &Value,
    ) -> Result<CommandIndexProbe, SQLError>;
}

pub fn scan(
    reads: &dyn QueryIndexRead,
    table: &str,
    field: &str,
    predicate: &Predicate,
    cancellation: &CancellationToken,
) -> Result<Option<PostingList>, SQLError> {
    let Some(indexed) = reads.value_index_scan(table, field, predicate)? else {
        return Ok(None);
    };
    let changes = reads.command_overlay_changes(table)?;
    if changes.as_ref().is_some_and(DocumentChanges::has_changes) {
        if let Predicate::Equals(value) = predicate {
            if crate::catalog::index::value::field_is_index_safe(value) {
                if matches!(value, Value::Null) {
                    return Ok(Some(PostingList::from_sorted_unchecked(Vec::new())));
                }
                let command = reads.exact_command_matches(table, field, value)?;
                return merge_exact(
                    indexed,
                    &changes.expect("changed rows").without_staged(),
                    command,
                    field,
                    predicate,
                    cancellation,
                )
                .map(Some);
            }
        }
    }
    merge_changes(indexed, changes, field, predicate, cancellation).map(Some)
}

fn merge_exact(
    indexed: PostingList,
    fixed: &DocumentChanges,
    command: CommandIndexProbe,
    field: &str,
    predicate: &Predicate,
    cancellation: &CancellationToken,
) -> Result<PostingList, SQLError> {
    let error = |error| storage_error("read indexed query changes", &error);
    let mut entries = Vec::with_capacity(indexed.len());
    for entry in indexed.entries() {
        cancellation.check()?;
        if !command
            .changes
            .contains_change(entry.doc_id)
            .map_err(error)?
            && !fixed.contains_change(entry.doc_id).map_err(error)?
        {
            entries.push(entry.clone());
        }
    }
    // Fixed portal/transaction versions retain their original source. Command keys above them are never obtained by traversing these changes.
    for change in fixed.changes() {
        cancellation.check()?;
        let (id, present) = change.map_err(error)?;
        if present
            && !command.changes.contains_change(id).map_err(error)?
            && predicate.evaluate(fixed.get_field(id, field).map_err(error)?.as_ref())
        {
            entries.push(PostingEntry::new(id, Payload::default()));
        }
    }
    for &id in command.matches.iter() {
        cancellation.check()?;
        entries.push(PostingEntry::new(id, Payload::default()));
    }
    entries.sort_unstable_by_key(|entry| entry.doc_id);
    Ok(PostingList::from_sorted_unchecked(entries))
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
    let error = |error| storage_error("read indexed command changes", &error);
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
        if present && predicate.evaluate(changes.get_field(id, field).map_err(error)?.as_ref()) {
            entries.push(PostingEntry::new(id, Payload::default()));
        }
    }
    entries.extend(candidates.cloned());
    Ok(PostingList::from_sorted_unchecked(entries))
}

#[cfg(test)]
mod tests;
