//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve index selection before command-mask reads and all row evaluation.

use super::{ids::IdentityBuilder, IndexCandidates, QueryIndexRead, SelectedIds};
use crate::query::document_changes::DocumentChanges;
use crate::storage_errors::storage_error;
use uqa_core::{CancellationToken, Predicate, Value};
use uqa_sql::SQLError;
use uqa_storage::DocumentStore;

pub(super) fn capture(
    context: &IndexCandidates<'_>,
    field: &str,
    predicate: &Predicate,
) -> Result<Option<SelectedIds>, SQLError> {
    capture_predicate(
        context.reads,
        context.table,
        field,
        predicate,
        context.command_visible,
        context.bytes,
        context.cancellation,
    )
}

pub(crate) fn capture_predicate(
    reads: &dyn QueryIndexRead,
    table: &str,
    field: &str,
    predicate: &Predicate,
    command_visible: bool,
    bytes: usize,
    cancellation: &CancellationToken,
) -> Result<Option<SelectedIds>, SQLError> {
    let mut builder = IdentityBuilder::new(bytes, cancellation);
    let mut visited = false;
    let supported = reads.visit_value_index(table, field, predicate, &mut |id| {
        visited = true;
        builder.push(id)
    })?;
    if !supported {
        return if visited {
            Err(SQLError::Internal(
                "declined index already returned candidates".into(),
            ))
        } else {
            Ok(None)
        };
    }
    let indexed = builder.finish(true)?;
    if !command_visible {
        return Ok(Some(indexed));
    }
    let Some(changes) = reads
        .command_overlay_changes(table)?
        .filter(DocumentChanges::has_changes)
    else {
        return Ok(Some(indexed));
    };
    let mut output = IdentityBuilder::new(bytes, cancellation);
    let error = |error| storage_error("read indexed query changes", &error);
    if let Predicate::Equals(value) = predicate {
        if crate::catalog::index::value::field_is_index_safe(value) {
            if matches!(value, Value::Null) {
                return output.finish(false).map(Some);
            }
            let mut command = IdentityBuilder::new(bytes, cancellation);
            let mask = reads
                .visit_exact_command_matches(table, field, value, &mut |id| command.push(id))?;
            let command = command.finish(true)?;
            let fixed = changes.without_staged();
            for id in indexed.cursor() {
                let id = id?;
                if !mask.contains_change(id).map_err(error)?
                    && !fixed.contains_change(id).map_err(error)?
                {
                    output.push(id)?;
                }
            }
            for change in fixed.changes() {
                cancellation.check()?;
                let (id, present) = change.map_err(error)?;
                if present
                    && !mask.contains_change(id).map_err(error)?
                    && predicate.evaluate(fixed.get_field(id, field).map_err(error)?.as_ref())
                {
                    output.push(id)?;
                }
            }
            for id in command.cursor() {
                output.push(id?)?;
            }
            return output.finish(true).map(Some);
        }
    }
    // Merge the complete selected support with changed identities in their original order; changed-field evaluation still precedes all row consumers.
    let error = |error| storage_error("read indexed command changes", &error);
    let mut candidates = indexed.cursor().peekable();
    for change in changes.changes() {
        cancellation.check()?;
        let (id, present) = change.map_err(error)?;
        while let Some(next) = candidates.peek() {
            match next {
                Ok(next) if *next >= id => break,
                _ => output.push(candidates.next().expect("peeked candidate")?)?,
            }
        }
        if matches!(candidates.peek(), Some(Ok(next)) if *next == id) {
            candidates.next().expect("peeked candidate")?;
        }
        if present && predicate.evaluate(changes.get_field(id, field).map_err(error)?.as_ref()) {
            output.push(id)?;
        }
    }
    for id in candidates {
        output.push(id?)?;
    }
    output.finish(false).map(Some)
}
