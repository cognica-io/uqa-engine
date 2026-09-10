//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    lock_mutation_target, prepare_referenced_key_delete_actions, BTreeSet, DocId,
    MutationLockTarget, PhysicalDocumentIdentity, PreparedDocumentDelete, ReferentialActionContext,
    ReferentialContext, SQLError, SQLParam,
};

pub fn prepare_document_delete<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    doc_id: DocId,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    referential_actions: &mut ReferentialActionContext,
    fire_row_triggers: bool,
) -> Result<Option<PreparedDocumentDelete>, SQLError> {
    let key = (table.to_string(), doc_id);
    if referential_actions.delete_stack.contains(&key) {
        return Ok(None);
    }
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
    let identity = PhysicalDocumentIdentity {
        table: table.to_string(),
        doc_id,
    };
    let target = match referential_actions.pending_document(&identity) {
        Some(Some(document)) => document.clone(),
        Some(None) => return Ok(None),
        None => {
            let Some(document) = context.constraints.reads.get_document(table, doc_id)? else {
                return Ok(None);
            };
            document
        }
    };
    if fire_row_triggers
        && crate::mutation::triggers::fire_before_row_triggers(
            &context.triggers,
            table,
            uqa_sql::ast::TriggerEvent::Delete,
            doc_id,
            Some(&target),
            None,
            &[],
        )?
        .is_none()
    {
        return Ok(None);
    }
    referential_actions
        .delete_stack
        .push((table.to_string(), doc_id));
    let actions = prepare_referenced_key_delete_actions(
        context,
        table,
        doc_id,
        &target,
        params,
        root_deletes,
        referential_actions,
    );
    referential_actions.delete_stack.pop();
    let prepared = PreparedDocumentDelete {
        table: table.to_string(),
        doc_id,
        document: target,
        actions: actions?,
    };
    referential_actions.record_pending_document(identity, None);
    Ok(Some(prepared))
}
