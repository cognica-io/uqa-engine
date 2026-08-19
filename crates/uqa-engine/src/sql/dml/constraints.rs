//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row, key, foreign-key, and referential-action validation.

use super::{
    coerce_to_column_type, dml_storage_error, document_vectors, eval_lowered_expression,
    lock_mutation_row, lock_mutation_target, missing_document_error, update_lock_strength, DocId,
    Document, Engine, ForeignKey, ForeignKeyAction, ForeignKeyMatch, MutationLockTarget,
    PreparedDocumentRewrite, SQLError, SQLParam, Value,
};
use sha2::{Digest, Sha256};

pub(in crate::sql) fn validate_document_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    validate_document_non_key_constraints(engine, table, document, params)?;
    validate_key_constraints(engine, table, document, ignored_doc_id)
}

pub(in crate::sql) fn validate_document_rewrite_constraints(
    engine: &Engine,
    table: &str,
    old_document: &Document,
    new_document: &Document,
    params: &[SQLParam],
    doc_id: DocId,
) -> Result<(), SQLError> {
    validate_document_non_key_constraints_with_old(
        engine,
        table,
        new_document,
        params,
        Some(old_document),
    )?;
    validate_key_constraints(engine, table, new_document, Some(doc_id))
}

pub(in crate::sql) fn validate_document_non_key_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    validate_document_non_key_constraints_with_old(engine, table, document, params, None)
}

fn validate_document_non_key_constraints_with_old(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
    old_document: Option<&Document>,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let check_constraints = engine
        .try_check_constraint_definitions(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?;
    let schema = uqa_execution::RowSchema::with_types(
        definitions
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        definitions
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let virtual_columns = definitions
        .iter()
        .filter(|column| {
            column.generated.as_ref().is_some_and(|generated| {
                generated.kind == uqa_sql::ast::GeneratedColumnKind::Virtual
            })
        })
        .collect::<Vec<_>>();
    let mut required_virtual_columns = std::collections::BTreeSet::new();
    for column in &virtual_columns {
        if column.not_null
            || check_constraints.iter().any(|constraint| {
                constraint.enforced
                    && crate::engine_table_storage::schema_expr_references_column(
                        &constraint.expr,
                        &column.name,
                    )
            })
        {
            required_virtual_columns.insert(column.name.clone());
        }
    }
    let logical_document = if required_virtual_columns.is_empty() {
        None
    } else {
        let mut logical_document = document.clone();
        crate::engine_generated::materialize_selected_virtual_generated_columns(
            &definitions,
            &mut logical_document,
            &required_virtual_columns,
        )?;
        Some(logical_document)
    };
    let document = logical_document.as_ref().unwrap_or(document);

    for col_def in &definitions {
        if !col_def.not_null || col_def.auto_increment {
            continue;
        }
        match document.get(&col_def.name) {
            Some(Value::Null) | None => {
                return Err(SQLError::TypeMismatch(format!(
                    "NOT NULL constraint violated: column `{}` in table `{table}`",
                    col_def.name
                )));
            }
            _ => {}
        }
    }

    for constraint in check_constraints {
        if !constraint.enforced {
            continue;
        }
        let result = crate::sql::scalar::eval_lowered_expression_with_schema(
            engine,
            &constraint.expr,
            document,
            &schema,
            params,
        )?;
        if !uqa_sql::expr::truthy(&result) {
            let label = constraint.name.unwrap_or_else(|| "<unnamed>".into());
            return Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: format!(
                    "new row for relation \"{table}\" violates check constraint \"{label}\""
                ),
            });
        }
    }

    lock_document_foreign_key_dependencies(engine, table, document, false, old_document)
}

/// Acquire every referenced-parent tuple lock that already exists without
/// rejecting a temporarily missing parent. INSERT uses this as a lock-only
/// preflight for all input rows before taking the backend writer; ordinary
/// constraint validation still runs in row order afterwards, so a
/// self-referencing row can see a parent inserted earlier by the same
/// statement and a genuinely missing parent still raises the normal error.
pub(in crate::sql) fn lock_existing_document_foreign_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
) -> Result<(), SQLError> {
    lock_document_foreign_key_dependencies(engine, table, document, true, None)
}

pub(in crate::sql) fn lock_existing_document_rewrite_foreign_key_dependencies(
    engine: &Engine,
    table: &str,
    old_document: &Document,
    new_document: &Document,
) -> Result<(), SQLError> {
    lock_document_foreign_key_dependencies(engine, table, new_document, true, Some(old_document))
}

fn lock_document_foreign_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
    allow_missing: bool,
    old_document: Option<&Document>,
) -> Result<(), SQLError> {
    for fk in engine
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        if !fk.enforced {
            continue;
        }
        if old_document.is_some_and(|old_document| {
            fk.local_columns.iter().all(|column| {
                old_document.get(column).cloned().unwrap_or(Value::Null)
                    == document.get(column).cloned().unwrap_or(Value::Null)
            })
        }) {
            continue;
        }
        let Some(local_values) = foreign_key_lookup_values(&fk, document)? else {
            continue;
        };
        let violation = || {
            let cols = fk.local_columns.join(", ");
            SQLError::TypeMismatch(format!(
                "FOREIGN KEY constraint violated: ({cols}) -> {}({}) has no matching row",
                fk.ref_table,
                fk.ref_columns.join(", ")
            ))
        };
        let mut hops = 0usize;
        loop {
            let Some(parent_id) =
                engine.find_conflict(&fk.ref_table, &fk.ref_columns, &local_values)?
            else {
                if allow_missing {
                    break;
                }
                return Err(violation());
            };
            // PostgreSQL 18 holds FOR KEY SHARE on the referenced row until
            // the referencing transaction ends. If the lookup waits, refresh
            // the READ COMMITTED snapshot and follow a delete/reinsert or key
            // rewrite until the tuple carrying the requested key is locked.
            let target = lock_mutation_target(
                engine,
                &fk.ref_table,
                &fk.ref_table,
                parent_id,
                uqa_sql::ast::LockStrength::ForKeyShare,
            )?;
            let MutationLockTarget::Present {
                doc_id: locked_parent,
                recheck,
            } = target
            else {
                engine.refresh_explicit_statement_snapshot()?;
                hops += 1;
                if hops > 64 {
                    return Err(SQLError::Internal(format!(
                        "foreign-key parent lookup for `{table}` did not converge"
                    )));
                }
                continue;
            };
            if recheck {
                engine.refresh_explicit_statement_snapshot()?;
            }
            match engine.find_conflict(&fk.ref_table, &fk.ref_columns, &local_values)? {
                Some(current_parent) if current_parent == locked_parent => break,
                None if allow_missing => break,
                None => return Err(violation()),
                Some(_) => {
                    hops += 1;
                    if hops > 64 {
                        return Err(SQLError::Internal(format!(
                            "foreign-key parent lookup for `{table}` did not converge"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

pub(in crate::sql) fn key_constraint_values(
    constraint: &uqa_sql::ast::TableKeyConstraint,
    document: &Document,
) -> Option<Vec<Value>> {
    let values: Vec<Value> = constraint
        .columns
        .iter()
        .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
        .collect();
    if constraint.kind == uqa_sql::ast::TableKeyConstraintKind::Unique
        && !constraint.nulls_not_distinct
        && values.iter().any(|value| matches!(value, Value::Null))
    {
        return None;
    }
    Some(values)
}

/// Reserve every UNIQUE / PRIMARY KEY value that a new row can publish, or
/// every such value changed by a rewrite, before the backend writer is held.
/// The reservation is the logical equivalent of `PostgreSQL`'s speculative
/// index-tuple wait: a deferred reader that cannot yet see another writer's
/// uncommitted row waits on the exact key, refreshes its snapshot, and only
/// then decides whether INSERT or ON CONFLICT applies.
pub(in crate::sql) fn lock_document_key_dependencies(
    engine: &Engine,
    table: &str,
    document: &Document,
    old_document: Option<&Document>,
) -> Result<Vec<crate::row_locks::RowLockAcquisition>, SQLError> {
    let canonical_table = engine
        .try_resolve_table_name(table)
        .map_err(|error| dml_storage_error("key-lock table resolution", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let constraints = engine
        .try_key_constraints(&canonical_table)
        .map_err(|error| dml_storage_error("key-lock constraint lookup", error))?;
    let mut lock_names = std::collections::BTreeSet::new();
    for constraint in constraints {
        let Some(values) = key_constraint_values(&constraint, document) else {
            continue;
        };
        if old_document.is_some_and(|old_document| {
            key_constraint_values(&constraint, old_document).as_ref() == Some(&values)
        }) {
            continue;
        }
        let key = uqa_execution::canonical_row_key(&values)
            .map_err(crate::sql::select::physical_exec_error)?;
        let mut digest = Sha256::new();
        digest.update(b"uqa-key-lock-v1");
        update_key_lock_digest(&mut digest, canonical_table.as_bytes())?;
        digest.update([match constraint.kind {
            uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => 0,
            uqa_sql::ast::TableKeyConstraintKind::Unique => 1,
        }]);
        digest.update([u8::from(constraint.nulls_not_distinct)]);
        for column in &constraint.columns {
            update_key_lock_digest(&mut digest, column.as_bytes())?;
        }
        update_key_lock_digest(&mut digest, &key)?;
        let digest = digest.finalize();
        lock_names.insert(format!("\0uqa-key-lock:{digest:x}"));
    }

    let mut acquisitions = Vec::new();
    let mut waited = false;
    for lock_name in lock_names {
        match engine.lock_row(
            &lock_name,
            0,
            uqa_sql::ast::LockStrength::ForUpdate,
            uqa_sql::ast::LockWait::Block,
            table,
        )? {
            crate::row_locks::LockAcquire::Granted {
                acquisition,
                waited: lock_waited,
                ..
            } => {
                waited |= lock_waited;
                acquisitions.extend(acquisition);
            }
            crate::row_locks::LockAcquire::Skipped => {
                return Err(SQLError::Internal(
                    "blocking key reservation unexpectedly skipped a key".into(),
                ));
            }
        }
    }
    if waited {
        engine.refresh_explicit_statement_snapshot()?;
    }
    Ok(acquisitions)
}

fn update_key_lock_digest(digest: &mut Sha256, part: &[u8]) -> Result<(), SQLError> {
    let len = u64::try_from(part.len())
        .map_err(|_| SQLError::Internal("key-lock digest part exceeds u64".into()))?;
    digest.update(len.to_be_bytes());
    digest.update(part);
    Ok(())
}

pub(in crate::sql) fn validate_key_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    ignored_doc_id: Option<DocId>,
) -> Result<(), SQLError> {
    for constraint in engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        let Some(values) = key_constraint_values(&constraint, document) else {
            continue;
        };
        let Some(conflict_id) = engine.find_conflict(table, &constraint.columns, &values)? else {
            continue;
        };
        if ignored_doc_id == Some(conflict_id) {
            continue;
        }
        let kind = match constraint.kind {
            uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
            uqa_sql::ast::TableKeyConstraintKind::Unique => "UNIQUE",
        };
        let name = constraint
            .name
            .as_deref()
            .map_or_else(String::new, |name| format!(" `{name}`"));
        return Err(SQLError::TypeMismatch(format!(
            "{kind} constraint{name} violated: duplicate value for columns ({}) in table `{table}`",
            constraint.columns.join(", ")
        )));
    }
    Ok(())
}

pub(in crate::sql) fn foreign_key_lookup_values(
    fk: &ForeignKey,
    document: &Document,
) -> Result<Option<Vec<Value>>, SQLError> {
    let local_values: Vec<Value> = fk
        .local_columns
        .iter()
        .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
        .collect();
    let null_count = local_values
        .iter()
        .filter(|value| matches!(value, Value::Null))
        .count();
    if null_count == 0 {
        return Ok(Some(local_values));
    }
    match fk.match_type {
        ForeignKeyMatch::Simple => Ok(None),
        ForeignKeyMatch::Full if null_count == local_values.len() => Ok(None),
        ForeignKeyMatch::Full => {
            let cols = fk.local_columns.join(", ");
            Err(SQLError::TypeMismatch(format!(
                "FOREIGN KEY MATCH FULL constraint violated: ({cols}) must be all NULL or all non-NULL"
            )))
        }
    }
}

pub(in crate::sql) fn rewrite_document_with_referential_actions(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    old_doc: &Document,
    new_doc: &mut Document,
    params: &[SQLParam],
) -> Result<DocId, SQLError> {
    let mut rewrite_stack = Vec::new();
    let Some(mut prepared) = prepare_document_rewrite(
        engine,
        table,
        doc_id,
        old_doc.clone(),
        new_doc.clone(),
        params,
        &mut rewrite_stack,
    )?
    else {
        return Ok(doc_id);
    };
    engine.prepare_explicit_transaction_writer()?;
    let rewritten_doc_id = apply_prepared_document_rewrite(engine, &mut prepared, params)?;
    new_doc.clone_from(&prepared.new_document);
    Ok(rewritten_doc_id)
}

/// Build the complete tuple-lock dependency tree for one rewrite while the
/// backend transaction is still deferred. The prepared documents retain
/// volatile SET DEFAULT results so the apply phase never re-evaluates them.
pub(in crate::sql) fn prepare_document_rewrite(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    old_document: Document,
    mut new_document: Document,
    params: &[SQLParam],
    rewrite_stack: &mut Vec<(String, DocId)>,
) -> Result<Option<PreparedDocumentRewrite>, SQLError> {
    let key = (table.to_string(), doc_id);
    if rewrite_stack.contains(&key) {
        return Ok(None);
    }
    engine.lock_relation(table, crate::row_locks::RelationLockMode::RowExclusive)?;
    let changed_columns = old_document
        .keys()
        .chain(new_document.keys())
        .filter(|column| old_document.get(*column) != new_document.get(*column))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    lock_mutation_row(
        engine,
        table,
        table,
        doc_id,
        update_lock_strength(engine, table, &changed_columns),
    )?;
    crate::sql::generated::refresh_stored_generated_columns(engine, table, &mut new_document)?;
    let _key_locks =
        lock_document_key_dependencies(engine, table, &new_document, Some(&old_document))?;
    lock_existing_document_rewrite_foreign_key_dependencies(
        engine,
        table,
        &old_document,
        &new_document,
    )?;
    rewrite_stack.push(key);
    let actions = prepare_referenced_key_update_actions(
        engine,
        table,
        &old_document,
        &new_document,
        params,
        rewrite_stack,
    );
    rewrite_stack.pop();
    Ok(Some(PreparedDocumentRewrite {
        table: table.to_string(),
        doc_id,
        old_document,
        new_document,
        actions: actions?,
    }))
}

pub(in crate::sql) fn apply_prepared_document_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
    params: &[SQLParam],
) -> Result<DocId, SQLError> {
    validate_document_rewrite_constraints(
        engine,
        &prepared.table,
        &prepared.old_document,
        &prepared.new_document,
        params,
        prepared.doc_id,
    )?;
    let rewritten_doc_id =
        match integer_primary_key_doc_id(engine, &prepared.table, &prepared.new_document)? {
            // An integer primary key names the row's doc_id slot; keep that
            // invariant when the key itself changes, or value -> doc_id
            // lookups (the unique fast path and FOREIGN KEY validation) read
            // the stale slot and miss the row.
            Some(new_id) if new_id != prepared.doc_id => {
                engine.delete_document(&prepared.table, prepared.doc_id)?;
                engine.add_prepared_document_with_vector_values(
                    &prepared.table,
                    new_id,
                    prepared.new_document.clone(),
                    document_vectors(engine, &prepared.table, &prepared.new_document)?,
                    true,
                )?;
                engine
                    .advance_next_id(&prepared.table, new_id)
                    .map_err(|err| dml_storage_error("UPDATE primary key", err))?;
                engine.note_row_rewritten(&prepared.table, prepared.doc_id, new_id);
                new_id
            }
            _ => {
                engine.rewrite_prepared_document(
                    &prepared.table,
                    prepared.doc_id,
                    prepared.new_document.clone(),
                )?;
                prepared.doc_id
            }
        };
    for action in &mut prepared.actions {
        apply_prepared_document_rewrite(engine, action, params)?;
    }
    Ok(rewritten_doc_id)
}

pub(in crate::sql) fn integer_primary_key_doc_id(
    engine: &Engine,
    table: &str,
    doc: &Document,
) -> Result<Option<DocId>, SQLError> {
    let Some(cols) = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("UPDATE primary key", err))?
    else {
        return Ok(None);
    };
    let Some(pk) = cols.iter().find(|c| c.primary_key && c.ty.is_integer()) else {
        return Ok(None);
    };
    Ok(match doc.get(&pk.name) {
        Some(Value::Int(v)) if *v >= 0 => Some(*v as DocId),
        _ => None,
    })
}

fn prepare_referenced_key_update_actions(
    engine: &Engine,
    table: &str,
    old_doc: &Document,
    new_doc: &Document,
    params: &[SQLParam],
    rewrite_stack: &mut Vec<(String, DocId)>,
) -> Result<Vec<PreparedDocumentRewrite>, SQLError> {
    let mut actions = Vec::new();
    for (ref_table, fk) in referrers_to_for_actions(engine, table)? {
        let old_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| old_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        let new_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| new_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if old_values == new_values || old_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        engine.lock_relation(&ref_table, crate::row_locks::RelationLockMode::RowExclusive)?;
        let referencing = referencing_rows(engine, &ref_table, &fk.local_columns, &old_values)?;
        for (child_id, _child_doc) in referencing {
            match fk.on_update {
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::TypeMismatch(format!(
                        "FOREIGN KEY constraint violated: UPDATE on `{table}` is referenced by `{ref_table}` ({} -> {})",
                        fk.local_columns.join(", "),
                        fk.ref_columns.join(", "),
                    )));
                }
                ForeignKeyAction::Cascade => {
                    let Some((child_id, child_doc)) = lock_referencing_child(
                        engine,
                        &ref_table,
                        child_id,
                        &fk.local_columns,
                        &fk.local_columns,
                        &old_values,
                    )?
                    else {
                        continue;
                    };
                    let mut updated = child_doc.clone();
                    for (col, value) in fk.local_columns.iter().zip(new_values.iter()) {
                        updated.insert(col.clone(), value.clone());
                    }
                    if let Some(prepared) = prepare_document_rewrite(
                        engine,
                        &ref_table,
                        child_id,
                        child_doc,
                        updated,
                        params,
                        rewrite_stack,
                    )? {
                        actions.push(prepared);
                    }
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let Some((child_id, child_doc)) = lock_referencing_child(
                        engine,
                        &ref_table,
                        child_id,
                        &fk.local_columns,
                        &fk.local_columns,
                        &old_values,
                    )?
                    else {
                        continue;
                    };
                    let mut updated = child_doc.clone();
                    apply_set_action_to_child(
                        engine,
                        &ref_table,
                        &child_doc,
                        &mut updated,
                        &fk.local_columns,
                        fk.on_update,
                        params,
                    )?;
                    if let Some(prepared) = prepare_document_rewrite(
                        engine,
                        &ref_table,
                        child_id,
                        child_doc,
                        updated,
                        params,
                        rewrite_stack,
                    )? {
                        actions.push(prepared);
                    }
                }
            }
        }
    }
    Ok(actions)
}

pub(in crate::sql) fn referrers_to_for_actions(
    engine: &Engine,
    table: &str,
) -> Result<Vec<(String, ForeignKey)>, SQLError> {
    engine
        .try_referrers_to(table)
        .map_err(|err| dml_storage_error("foreign-key lookup", err))
}

/// Lock one referencing child row for a referential action and refetch it
/// after the wait. Returns `None` when the child vanished or its foreign-key
/// columns no longer reference the parent key that triggered the action, so
/// the action skips it exactly like `PostgreSQL` after an `EvalPlanQual`
/// recheck of the referencing row.
pub(in crate::sql) fn lock_referencing_child(
    engine: &Engine,
    ref_table: &str,
    child_id: DocId,
    lock_columns: &[String],
    key_columns: &[String],
    key_values: &[Value],
) -> Result<Option<(DocId, Document)>, SQLError> {
    let target = lock_mutation_target(
        engine,
        ref_table,
        ref_table,
        child_id,
        update_lock_strength(engine, ref_table, lock_columns),
    )?;
    let MutationLockTarget::Present {
        doc_id: child_id,
        recheck,
    } = target
    else {
        return Ok(None);
    };
    if recheck {
        engine.refresh_explicit_statement_snapshot()?;
    }
    let Some(child_doc) = engine.get_document(ref_table, child_id)? else {
        return Ok(None);
    };
    let still_references = key_columns
        .iter()
        .zip(key_values)
        .all(|(column, value)| child_doc.get(column).cloned().unwrap_or(Value::Null) == *value);
    Ok(still_references.then_some((child_id, child_doc)))
}

pub(in crate::sql) fn referencing_rows(
    engine: &Engine,
    table: &str,
    local_columns: &[String],
    key_values: &[Value],
) -> Result<Vec<(DocId, Document)>, SQLError> {
    if local_columns.is_empty() || local_columns.len() != key_values.len() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for doc_id in engine.table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
            return Err(missing_document_error(
                "foreign-key reference scan",
                table,
                doc_id,
            ));
        };
        let matches = local_columns
            .iter()
            .zip(key_values.iter())
            .all(|(col, want)| doc.get(col).cloned().unwrap_or(Value::Null) == *want);
        if matches {
            out.push((doc_id, doc));
        }
    }
    Ok(out)
}

pub(in crate::sql) fn apply_set_action_to_child(
    engine: &Engine,
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
                if let Some(expr) = engine
                    .try_column_default_expr(table, column)
                    .map_err(|err| dml_storage_error("referential SET DEFAULT", err))?
                {
                    eval_lowered_expression(engine, &expr, Some(old_doc), params)?
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
        let value = coerce_to_column_type(engine, table, column, value)?;
        new_doc.insert(column.clone(), value);
    }
    Ok(())
}
