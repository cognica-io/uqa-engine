//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict resolution, identity extraction, and RETURNING assembly.

use super::{
    build_projection_row_with_ctes, coerce_to_column_type, dml_storage_error, doc_id_value,
    eval_mutation_expr, expand_star_columns, key_constraint_values, missing_document_error,
    projection_columns, rewrite_document_with_referential_actions, BTreeSet, ConflictActionPlan,
    ConflictPlan, CteScope, DocId, Document, Engine, ProjectionPlan, ResultRow, SQLError, SQLParam,
    SQLResult, Value, DOC_ID_COLUMN,
};
use uqa_sql::ast::ReturningAliases;

pub(in crate::sql) fn find_insert_conflict(
    engine: &Engine,
    table: &str,
    on_conflict: &ConflictPlan,
    document: &Document,
) -> Result<Option<DocId>, SQLError> {
    let constraints = engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("INSERT conflict lookup", err))?;
    if !on_conflict.conflict_columns.is_empty() {
        let target: BTreeSet<&str> = on_conflict
            .conflict_columns
            .iter()
            .map(String::as_str)
            .collect();
        if target.len() != on_conflict.conflict_columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "ON CONFLICT target ({}) names a column more than once",
                on_conflict.conflict_columns.join(", ")
            )));
        }
        let constraint = constraints
            .iter()
            .find(|constraint| {
                constraint.columns.len() == target.len()
                    && constraint
                        .columns
                        .iter()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>()
                        == target
            })
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "ON CONFLICT target ({}) does not match a PRIMARY KEY or UNIQUE constraint",
                    on_conflict.conflict_columns.join(", ")
                ))
            })?;
        let Some(conflict_values) = key_constraint_values(constraint, document) else {
            return Ok(None);
        };
        return engine.find_conflict(table, &constraint.columns, &conflict_values);
    }

    for constraint in &constraints {
        let Some(values) = key_constraint_values(constraint, document) else {
            continue;
        };
        if let Some(doc_id) = engine.find_conflict(table, &constraint.columns, &values)? {
            return Ok(Some(doc_id));
        }
    }
    Ok(None)
}

pub(in crate::sql) enum InsertConflictResolution {
    Insert,
    Skip,
    Updated {
        old_doc_id: DocId,
        doc_id: DocId,
        old_document: Document,
        document: Document,
    },
}

pub(in crate::sql) fn resolve_insert_conflict(
    engine: &Engine,
    table: &str,
    on_conflict: &ConflictPlan,
    document: &Document,
    params: &[SQLParam],
    scope: &CteScope,
) -> Result<InsertConflictResolution, SQLError> {
    let Some(existing_id) = find_insert_conflict(engine, table, on_conflict, document)? else {
        return Ok(InsertConflictResolution::Insert);
    };
    match &on_conflict.action {
        ConflictActionPlan::Nothing => Ok(InsertConflictResolution::Skip),
        ConflictActionPlan::Update {
            assignments,
            predicate,
        } => {
            let existing_doc = engine
                .get_document(table, existing_id)?
                .ok_or_else(|| missing_document_error("INSERT ON CONFLICT", table, existing_id))?;
            let mut conflict_ctx_doc = existing_doc.clone();
            for (column, value) in &existing_doc {
                conflict_ctx_doc.insert(format!("{table}.{column}"), value.clone());
            }
            for (column, value) in document {
                conflict_ctx_doc.insert(format!("excluded.{column}"), value.clone());
            }
            if let Some(predicate) = predicate {
                let keep =
                    eval_mutation_expr(engine, scope, predicate, Some(&conflict_ctx_doc), params)?;
                if !uqa_sql::expr::truthy(&keep) {
                    return Ok(InsertConflictResolution::Skip);
                }
            }
            let mut updated_doc = existing_doc.clone();
            for assignment in assignments {
                let value = coerce_to_column_type(
                    engine,
                    table,
                    &assignment.column,
                    eval_mutation_expr(
                        engine,
                        scope,
                        &assignment.value,
                        Some(&conflict_ctx_doc),
                        params,
                    )?,
                )?;
                updated_doc.insert(assignment.column.clone(), value);
            }
            let rewritten_doc_id = rewrite_document_with_referential_actions(
                engine,
                table,
                existing_id,
                &existing_doc,
                updated_doc.clone(),
                params,
            )?;
            Ok(InsertConflictResolution::Updated {
                old_doc_id: existing_id,
                doc_id: rewritten_doc_id,
                old_document: existing_doc,
                document: updated_doc,
            })
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::sql) struct ReturningRowImage<'a> {
    pub doc_id: DocId,
    pub document: &'a Document,
}

#[derive(Clone, Copy)]
pub(in crate::sql) struct ReturningRowImages<'a> {
    pub old: Option<ReturningRowImage<'a>>,
    pub new: Option<ReturningRowImage<'a>>,
}

pub(in crate::sql) fn returning_row_context(
    engine: &Engine,
    table: &str,
    images: ReturningRowImages<'_>,
    aliases: &ReturningAliases,
) -> Result<ResultRow, SQLError> {
    let current = images.new.or(images.old).ok_or_else(|| {
        SQLError::Internal(format!(
            "RETURNING for table `{table}` has neither an old nor a new row image"
        ))
    })?;
    let mut row_doc = current.document.clone();
    row_doc.insert(DOC_ID_COLUMN.into(), doc_id_value(current.doc_id)?);

    let mut columns = engine
        .try_table_columns(table)
        .map_err(|err| dml_storage_error("RETURNING schema lookup", err))?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if let Some(image) = images.old {
        columns.extend(image.document.keys().cloned());
    }
    if let Some(image) = images.new {
        columns.extend(image.document.keys().cloned());
    }
    columns.insert(DOC_ID_COLUMN.into());

    for column in columns {
        let old_value = row_image_value(images.old, &column)?;
        let new_value = row_image_value(images.new, &column)?;
        row_doc.insert(format!("{}.{}", aliases.old, column), old_value);
        row_doc.insert(format!("{}.{}", aliases.new, column), new_value);
    }
    Ok(row_doc)
}

fn row_image_value(image: Option<ReturningRowImage<'_>>, column: &str) -> Result<Value, SQLError> {
    let Some(image) = image else {
        return Ok(Value::Null);
    };
    if column == DOC_ID_COLUMN {
        return doc_id_value(image.doc_id);
    }
    Ok(image.document.get(column).cloned().unwrap_or(Value::Null))
}

pub(in crate::sql) fn build_returning_row(
    engine: &Engine,
    table: &str,
    images: ReturningRowImages<'_>,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<ResultRow, SQLError> {
    let row_doc = returning_row_context(engine, table, images, aliases)?;
    let doc_id = images.new.or(images.old).map_or(0, |image| image.doc_id);
    build_projection_row_with_ctes(engine, &row_doc, returning, params, ctes).map_err(|err| {
        SQLError::Internal(format!(
            "RETURNING projection failed for table `{table}` doc {doc_id}: {err}"
        ))
    })
}

pub(in crate::sql) fn dml_returning_result(
    engine: &Engine,
    table: &str,
    returning: &[ProjectionPlan],
    rows: Vec<ResultRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    Ok(SQLResult {
        columns: expand_star_columns(
            projection_columns(returning),
            returning,
            engine,
            Some(table),
        )?,
        rows,
        affected_rows,
    })
}

pub(in crate::sql) fn document_supplied_id(
    document: &Document,
    id_column: &str,
    auto_increment: bool,
) -> Result<Option<DocId>, SQLError> {
    match document.get(id_column) {
        Some(Value::Int(value)) if *value >= 0 => Ok(Some(*value as DocId)),
        Some(Value::Null) | None => Ok(None),
        Some(other) if auto_increment => Err(SQLError::TypeMismatch(format!(
            "auto-increment id must be an integer, got {other:?}"
        ))),
        Some(_) => Ok(None),
    }
}

// -------------------------------------------------------------------------

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------
