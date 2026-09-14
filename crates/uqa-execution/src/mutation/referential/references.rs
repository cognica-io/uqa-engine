//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    dml_storage_error, lock_physical_mutation_target, missing_document_error, update_lock_strength,
    Document, ForeignKey, ForeignKeyAction, ForeignKeyComparison, PhysicalDocumentIdentity,
    PhysicalMutationLockTarget, ReferentialActionContext, ReferentialContext, SQLError, SQLParam,
    Value,
};

/// Lock one referencing child row for a referential action and refetch it after the wait. Returns `None` when the child vanished or its foreign-key columns no longer reference the parent key that triggered the action, so the action skips it exactly like `PostgreSQL` after an `EvalPlanQual` recheck of the referencing row.
pub struct ReferencingChildLock<'a> {
    pub ref_table: &'a str,
    pub child: &'a PhysicalDocumentIdentity,
    pub lock_columns: &'a [String],
    pub foreign_key: &'a ForeignKey,
    pub comparison: &'a ForeignKeyComparison,
    pub expected: &'a [Value],
}

pub fn lock_referencing_child<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    request: ReferencingChildLock<'_>,
    referential_actions: &ReferentialActionContext,
) -> Result<Option<(PhysicalDocumentIdentity, Document)>, SQLError> {
    let ReferencingChildLock {
        ref_table,
        child,
        lock_columns,
        foreign_key,
        comparison,
        expected,
    } = request;
    let target = lock_physical_mutation_target(
        context.locking.session,
        &child.table,
        ref_table,
        child.doc_id,
        update_lock_strength(context.locking.catalog, &child.table, lock_columns),
    )?;
    let PhysicalMutationLockTarget::Present { identity, recheck } = target else {
        return Ok(None);
    };
    if recheck {
        context
            .constraints
            .transactions
            .refresh_explicit_statement_snapshot()?;
    }
    let child_doc = match referential_actions.pending_document(&identity) {
        Some(Some(document)) => document.clone(),
        Some(None) => return Ok(None),
        None => {
            let Some(document) = context
                .locking
                .rows
                .get_document_for_mutation(&identity.table, identity.doc_id)?
            else {
                return Ok(None);
            };
            document
        }
    };
    let actual = foreign_key
        .local_columns
        .iter()
        .map(|column| child_doc.get(column).cloned().unwrap_or(Value::Null))
        .collect();
    let actual = comparison.normalize(actual)?;
    Ok((actual == expected).then_some((identity, child_doc)))
}

pub fn referencing_rows<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    fk: &ForeignKey,
    comparison: &ForeignKeyComparison,
    expected: &[Value],
    referential_actions: &ReferentialActionContext,
    action: ForeignKeyAction,
) -> Result<Vec<(PhysicalDocumentIdentity, Document)>, SQLError> {
    let mut out = Vec::new();
    let snapshot = super::snapshots::ReferenceSnapshot::new(context)?;
    for physical_table in context
        .constraints
        .catalog
        .hierarchy_scan_tables(table, true)?
    {
        let rows = snapshot.table(&physical_table)?;
        for doc_id in rows.doc_ids()? {
            let identity = PhysicalDocumentIdentity {
                table: physical_table.clone(),
                doc_id,
            };
            let doc = match referential_actions.pending_document(&identity) {
                Some(Some(document)) => document.clone(),
                Some(None) => continue,
                None => {
                    let Some(document) = rows.document(doc_id)? else {
                        return Err(missing_document_error(
                            "foreign-key reference scan",
                            &physical_table,
                            doc_id,
                        ));
                    };
                    document
                }
            };
            let values = fk
                .local_columns
                .iter()
                .map(|column| doc.get(column).cloned().unwrap_or(Value::Null))
                .collect();
            if comparison.normalize(values)? == expected {
                if matches!(
                    action,
                    ForeignKeyAction::Cascade
                        | ForeignKeyAction::SetNull
                        | ForeignKeyAction::SetDefault
                ) && referential_actions.pending_document(&identity).is_none()
                {
                    rows.check_visible(doc_id)?;
                }
                out.push((identity, doc));
            }
        }
    }
    Ok(out)
}

pub fn apply_set_action_to_child<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    old_doc: &Document,
    new_doc: &mut Document,
    columns: &[String],
    action: ForeignKeyAction,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for column in columns {
        let value = match action {
            ForeignKeyAction::SetNull => Value::Null,
            ForeignKeyAction::SetDefault => {
                if let Some(expr) = context
                    .assignment
                    .columns
                    .try_column_insert_default_expr(table, column)
                    .map_err(|err| dml_storage_error("referential SET DEFAULT", err))?
                {
                    crate::query::catalog_expression::eval_lowered_expression(
                        context.assignment.expressions.expressions,
                        context.assignment.scopes.current_routine_scope(),
                        &expr,
                        Some(old_doc),
                        params,
                    )?
                } else {
                    Value::Null
                }
            }
            ForeignKeyAction::NoAction | ForeignKeyAction::Restrict | ForeignKeyAction::Cascade => {
                return Err(SQLError::Internal(format!(
                    "invalid SET action helper for `{action:?}`"
                )));
            }
        };
        let value = uqa_sql::assignment::columns::coerce_to_column_type(
            context.assignment.assignment,
            context.assignment.columns,
            table,
            column,
            value,
        )?;
        new_doc.insert(column.clone(), value);
    }
    Ok(())
}
