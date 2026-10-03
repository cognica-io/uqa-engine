//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    defer_deleted_key_checks, lock_mutation_target, BTreeSet, DocId, MutationLockTarget,
    PreparedDocumentDelete, ReferentialContext, SQLError,
};

/// Lock the row that `table` holds at `doc_id` and prepare its delete, firing its BEFORE ROW triggers when `fire_row_triggers` holds, and leave for the transaction the checks of the deferred `NO ACTION` keys it removes. `root_deletes` holds the rows the same statement deletes. `None` when the row is gone or a BEFORE ROW trigger skipped it.
pub fn prepare_document_delete<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    doc_id: DocId,
    root_deletes: &BTreeSet<(String, DocId)>,
    fire_row_triggers: bool,
) -> Result<Option<PreparedDocumentDelete>, SQLError> {
    context
        .locking
        .session
        .lock_relation(table, crate::row_locks::RelationLockMode::RowExclusive)?;
    let target = lock_mutation_target(
        context.locking.session,
        table,
        table,
        doc_id,
        uqa_sql::ast::LockStrength::ForUpdate,
    )?;
    let MutationLockTarget::Present { doc_id, recheck } = target else {
        return Ok(None);
    };
    if recheck {
        context
            .constraints
            .transactions
            .refresh_explicit_statement_snapshot()?;
    }
    let Some(document) = context.constraints.reads.get_document(table, doc_id)? else {
        return Ok(None);
    };
    if fire_row_triggers
        && crate::mutation::triggers::fire_before_row_triggers(
            &context.triggers,
            table,
            uqa_sql::ast::TriggerEvent::Delete,
            doc_id,
            Some(&document),
            None,
            &[],
        )?
        .is_none()
    {
        return Ok(None);
    }
    defer_deleted_key_checks(context, table, doc_id, &document, root_deletes)?;
    Ok(Some(PreparedDocumentDelete {
        table: table.to_string(),
        doc_id,
        document,
    }))
}
