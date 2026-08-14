//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict resolution, identity extraction, and RETURNING assembly.

use super::{
    bind_projection_output_schema, build_projection_row_with_ctes, dml_storage_error, doc_id_value,
    eval_mutation_assignment, eval_mutation_expr, key_constraint_values, missing_document_error,
    rewrite_document_with_referential_actions, BTreeSet, ConflictActionPlan, ConflictPlan,
    CteScope, DocId, Document, Engine, MutationAssignmentTarget, ProjectionPlan, ResultRow,
    SQLError, SQLParam, SQLResult, Value, DOC_ID_COLUMN,
};
use uqa_execution::RowSchema;
use uqa_sql::ast::ReturningAliases;
use uqa_sql::expr::RowLookup;

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
            let definitions = engine
                .try_describe_table(table)
                .map_err(|error| dml_storage_error("INSERT EXCLUDED schema", error))?
                .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
            let mut excluded_document = document.clone();
            crate::engine_generated::materialize_virtual_generated_columns(
                &definitions,
                &mut excluded_document,
            )?;
            for (column, value) in &excluded_document {
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
                let value = eval_mutation_assignment(
                    engine,
                    scope,
                    MutationAssignmentTarget {
                        table,
                        column: &assignment.column,
                        action: "INSERT ON CONFLICT DO UPDATE",
                    },
                    &assignment.value,
                    Some(&conflict_ctx_doc),
                    params,
                )?;
                if let Some(value) = value {
                    updated_doc.insert(assignment.column.clone(), value);
                } else {
                    updated_doc.remove(&assignment.column);
                }
            }
            let rewritten_doc_id = rewrite_document_with_referential_actions(
                engine,
                table,
                existing_id,
                &existing_doc,
                &mut updated_doc,
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
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut row_doc = current.document.clone();
    crate::engine_generated::materialize_virtual_generated_columns(&definitions, &mut row_doc)?;
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

    let local_table = table.rsplit_once('.').map_or(table, |(_, name)| name);
    for column in columns {
        let current_value = row_image_value(Some(current), &column, &definitions)?;
        let old_value = row_image_value(images.old, &column, &definitions)?;
        let new_value = row_image_value(images.new, &column, &definitions)?;
        row_doc.insert(column.clone(), current_value.clone());
        row_doc.insert(format!("{table}.{column}"), current_value.clone());
        row_doc.insert(format!("{local_table}.{column}"), current_value);
        row_doc.insert(format!("{}.{}", aliases.old, column), old_value);
        row_doc.insert(format!("{}.{}", aliases.new, column), new_value);
    }
    Ok(row_doc)
}

fn row_image_value(
    image: Option<ReturningRowImage<'_>>,
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Result<Value, SQLError> {
    let Some(image) = image else {
        return Ok(Value::Null);
    };
    if column == DOC_ID_COLUMN {
        return doc_id_value(image.doc_id);
    }
    if definitions.iter().any(|definition| {
        definition.name == column && definition.primary_key && definition.ty.is_integer()
    }) {
        return doc_id_value(image.doc_id);
    }
    let mut document = image.document.clone();
    crate::engine_generated::materialize_virtual_generated_columns(definitions, &mut document)?;
    Ok(document.get(column).cloned().unwrap_or(Value::Null))
}

pub(in crate::sql) struct ReturningProjectionRow<'a> {
    pub table: &'a str,
    pub images: ReturningRowImages<'a>,
    pub aliases: &'a ReturningAliases,
    pub context: Option<&'a dyn RowLookup>,
}

pub(in crate::sql) fn build_returning_row(
    engine: &Engine,
    input: ReturningProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<ResultRow, SQLError> {
    let mut row_doc = returning_row_context(engine, input.table, input.images, input.aliases)?;
    if let Some(context) = input.context {
        context.visit_lookup_columns(&mut |column, value| {
            row_doc
                .entry(column.to_string())
                .or_insert_with(|| value.clone());
        });
    }
    let projections = expanded_returning_projections(engine, input.table, returning)?;
    let doc_id = input
        .images
        .new
        .or(input.images.old)
        .map_or(0, |image| image.doc_id);
    build_projection_row_with_ctes(engine, &row_doc, &projections, params, ctes).map_err(|err| {
        SQLError::Internal(format!(
            "RETURNING projection failed for table `{}` doc {doc_id}: {err}",
            input.table
        ))
    })
}

pub(in crate::sql) struct DmlReturningShape<'a> {
    pub table: &'a str,
    pub target_qualifier: Option<&'a str>,
    pub aliases: &'a ReturningAliases,
    pub returning: &'a [ProjectionPlan],
    pub params: &'a [SQLParam],
    pub ctes: &'a CteScope,
    pub supplemental_schema: Option<&'a RowSchema>,
}

pub(in crate::sql) fn dml_returning_result(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    rows: Vec<ResultRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    let star_schema = returning_target_schema(engine, shape.table)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        shape.table,
        shape.target_qualifier,
        shape.aliases,
        shape.supplemental_schema,
    );
    let output = bind_projection_output_schema(
        engine,
        shape.returning,
        &expression_schema,
        &star_schema,
        &shape.ctes.scalar_subqueries,
        shape.params,
        shape.ctes,
    )?;
    Ok(SQLResult {
        columns: output.columns().to_vec(),
        column_types: output.column_types().to_vec(),
        rows,
        positional_rows: None,
        affected_rows,
    })
}

fn returning_target_schema(engine: &Engine, table: &str) -> Result<RowSchema, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    if definitions.is_empty() {
        let columns = engine
            .try_table_columns(table)
            .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?;
        let width = columns.len();
        return Ok(RowSchema::with_types(columns, vec![None; width]));
    }
    let columns = definitions
        .iter()
        .map(|definition| definition.name.clone())
        .collect();
    let types = definitions
        .into_iter()
        .map(|definition| Some(definition.ty))
        .collect();
    Ok(RowSchema::with_types(columns, types))
}

fn returning_expression_schema(
    target: &RowSchema,
    table: &str,
    target_qualifier: Option<&str>,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> RowSchema {
    let with_doc_id = RowSchema::append_typed(
        target,
        &[(
            DOC_ID_COLUMN.into(),
            Some(uqa_sql::ast::ColumnType::BigInteger),
        )],
    );
    let local_table = table.rsplit_once('.').map_or(table, |(_, name)| name);
    let mut qualifiers = vec![
        table,
        local_table,
        aliases.old.as_str(),
        aliases.new.as_str(),
    ];
    if let Some(target_qualifier) = target_qualifier {
        qualifiers.push(target_qualifier);
    }
    qualifiers.sort_unstable();
    qualifiers.dedup();
    let aliases = qualifiers
        .into_iter()
        .flat_map(|qualifier| {
            with_doc_id
                .columns()
                .iter()
                .map(move |column| (format!("{qualifier}.{column}"), column.clone()))
        })
        .collect::<Vec<_>>();
    let target = RowSchema::with_lookup_aliases(&with_doc_id, &aliases);
    supplemental.map_or(target.clone(), |source| {
        RowSchema::join(&target, source, std::iter::empty())
    })
}

pub(in crate::sql) fn expanded_returning_projections(
    engine: &Engine,
    table: &str,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let columns = returning_target_schema(engine, table)?.columns().to_vec();
    let mut projections = Vec::with_capacity(returning.len().max(columns.len()));
    for projection in returning {
        if matches!(projection.expr, uqa_execution::ScalarExpr::Star) {
            projections.extend(columns.iter().map(|column| ProjectionPlan {
                expr: uqa_execution::ScalarExpr::Column(column.clone()),
                alias: Some(column.clone()),
            }));
        } else {
            projections.push(projection.clone());
        }
    }
    Ok(projections)
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
