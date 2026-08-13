//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row, key, foreign-key, and referential-action validation.

use super::{
    coerce_to_column_type, dml_storage_error, document_vectors, eval_lowered_expression,
    missing_document_error, DocId, Document, Engine, ForeignKey, ForeignKeyAction, ForeignKeyMatch,
    SQLError, SQLParam, Value,
};

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

pub(in crate::sql) fn validate_document_non_key_constraints(
    engine: &Engine,
    table: &str,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let check_constraints = engine
        .try_check_constraint_definitions(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?;
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
        let result = eval_lowered_expression(engine, &constraint.expr, Some(document), params)?;
        if !uqa_sql::expr::truthy(&result) {
            let label = constraint.name.unwrap_or_else(|| "<unnamed>".into());
            return Err(SQLError::TypeMismatch(format!(
                "CHECK constraint `{label}` violated in table `{table}`"
            )));
        }
    }

    for fk in engine
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("constraint validation", err))?
    {
        if !fk.enforced {
            continue;
        }
        let Some(local_values) = foreign_key_lookup_values(&fk, document)? else {
            continue;
        };
        if engine
            .find_conflict(&fk.ref_table, &fk.ref_columns, &local_values)?
            .is_none()
        {
            let cols = fk.local_columns.join(", ");
            return Err(SQLError::TypeMismatch(format!(
                "FOREIGN KEY constraint violated: ({cols}) -> {}({}) has no matching row",
                fk.ref_table,
                fk.ref_columns.join(", ")
            )));
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
    crate::sql::generated::refresh_stored_generated_columns(engine, table, new_doc)?;
    validate_document_constraints(engine, table, new_doc, params, Some(doc_id))?;
    let rewritten_doc_id = match integer_primary_key_doc_id(engine, table, new_doc)? {
        // An integer primary key names the row's doc_id slot; keep that
        // invariant when the key itself changes, or value -> doc_id
        // lookups (the unique fast path and FOREIGN KEY validation) read
        // the stale slot and miss the row.
        Some(new_id) if new_id != doc_id => {
            engine.delete_document(table, doc_id)?;
            engine.add_prepared_document_with_vector_values(
                table,
                new_id,
                new_doc.clone(),
                document_vectors(engine, table, new_doc)?,
                true,
            )?;
            engine
                .advance_next_id(table, new_id)
                .map_err(|err| dml_storage_error("UPDATE primary key", err))?;
            new_id
        }
        _ => {
            engine.rewrite_prepared_document(table, doc_id, new_doc.clone())?;
            doc_id
        }
    };
    apply_referenced_key_update_actions(engine, table, old_doc, new_doc, params)?;
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
    let Some(pk) = cols
        .iter()
        .find(|c| c.primary_key && matches!(c.ty, uqa_sql::ast::ColumnType::Integer))
    else {
        return Ok(None);
    };
    Ok(match doc.get(&pk.name) {
        Some(Value::Int(v)) if *v >= 0 => Some(*v as DocId),
        _ => None,
    })
}

pub(in crate::sql) fn apply_referenced_key_update_actions(
    engine: &Engine,
    table: &str,
    old_doc: &Document,
    new_doc: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
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
        let referencing = referencing_rows(engine, &ref_table, &fk.local_columns, &old_values)?;
        for (child_id, child_doc) in referencing {
            match fk.on_update {
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::TypeMismatch(format!(
                        "FOREIGN KEY constraint violated: UPDATE on `{table}` is referenced by `{ref_table}` ({} -> {})",
                        fk.local_columns.join(", "),
                        fk.ref_columns.join(", "),
                    )));
                }
                ForeignKeyAction::Cascade => {
                    let mut updated = child_doc.clone();
                    for (col, value) in fk.local_columns.iter().zip(new_values.iter()) {
                        updated.insert(col.clone(), value.clone());
                    }
                    rewrite_document_with_referential_actions(
                        engine,
                        &ref_table,
                        child_id,
                        &child_doc,
                        &mut updated,
                        params,
                    )?;
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
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
                    rewrite_document_with_referential_actions(
                        engine,
                        &ref_table,
                        child_id,
                        &child_doc,
                        &mut updated,
                        params,
                    )?;
                }
            }
        }
    }
    Ok(())
}

pub(in crate::sql) fn referrers_to_for_actions(
    engine: &Engine,
    table: &str,
) -> Result<Vec<(String, ForeignKey)>, SQLError> {
    engine
        .try_referrers_to(table)
        .map_err(|err| dml_storage_error("foreign-key lookup", err))
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
