//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict resolution, identity extraction, and RETURNING assembly.

use super::{
    bind_projection_output_schema, build_projection_physical_row_with_ctes,
    dml_append_hidden_qualified_row, dml_storage_error, dml_target_row, doc_id_value,
    eval_mutation_assignment, eval_mutation_expr, key_constraint_values, missing_document_error,
    rewrite_document_with_referential_actions, BTreeSet, ConflictActionPlan, ConflictPlan,
    CteScope, DocId, Document, Engine, MutationAssignmentTarget, ProjectionPlan, SQLError,
    SQLParam, SQLResult, Value, DOC_ID_COLUMN,
};
use uqa_execution::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema};
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
    target_qualifier: &str,
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
            let target_row =
                dml_target_row(engine, table, target_qualifier, existing_id, &existing_doc)?;
            let definitions = engine
                .try_describe_table(table)
                .map_err(|error| dml_storage_error("INSERT EXCLUDED schema", error))?
                .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
            let mut excluded_document = document.clone();
            crate::engine_generated::materialize_virtual_generated_columns(
                &definitions,
                &mut excluded_document,
            )?;
            let excluded_columns = if definitions.is_empty() {
                excluded_document.keys().cloned().collect::<Vec<_>>()
            } else {
                definitions
                    .iter()
                    .map(|definition| definition.name.clone())
                    .collect::<Vec<_>>()
            };
            let excluded_types = excluded_columns
                .iter()
                .map(|column| {
                    definitions
                        .iter()
                        .find(|definition| definition.name == *column)
                        .map(|definition| definition.ty.clone())
                })
                .collect::<Vec<_>>();
            let excluded_values = excluded_columns
                .iter()
                .map(|column| {
                    excluded_document
                        .get(column)
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .collect();
            let conflict_row = dml_append_hidden_qualified_row(
                &target_row,
                "excluded",
                &excluded_columns,
                &excluded_types,
                excluded_values,
            );
            if let Some(predicate) = predicate {
                let keep =
                    eval_mutation_expr(engine, scope, predicate, Some(&conflict_row), params)?;
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
                    Some(&conflict_row),
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

pub(in crate::sql) fn validate_returning_alias_relations(
    target_qualifier: &str,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let mut relation_names = BTreeSet::from([target_qualifier]);
    for (alias, explicit) in [
        (aliases.old.as_str(), aliases.old_explicit),
        (aliases.new.as_str(), aliases.new_explicit),
    ] {
        if !explicit {
            continue;
        }
        if relation_names.contains(alias)
            || supplemental.is_some_and(|schema| schema.has_qualifier(alias))
        {
            return Err(SQLError::Routine {
                sqlstate: "42712".into(),
                message: format!("table name \"{alias}\" specified more than once"),
            });
        }
        relation_names.insert(alias);
    }
    Ok(())
}

pub(in crate::sql) fn returning_row_context(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    images: ReturningRowImages<'_>,
    aliases: &ReturningAliases,
) -> Result<OwnedPhysicalRow, SQLError> {
    let current = images.new.or(images.old).ok_or_else(|| {
        SQLError::Internal(format!(
            "RETURNING for table `{table}` has neither an old nor a new row image"
        ))
    })?;
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("RETURNING schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let target = returning_target_schema(engine, table)?;
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(uqa_sql::ast::ColumnType::BigInteger));
    }
    let schema = returning_context_schema(&columns, &types, target_qualifier, aliases);
    let current_values = returning_image_values(Some(current), &columns, &definitions)?;
    let old_values = returning_image_values(images.old, &columns, &definitions)?;
    let new_values = returning_image_values(images.new, &columns, &definitions)?;
    let values = current_values
        .into_iter()
        .chain(old_values)
        .chain(new_values)
        .collect();
    Ok(OwnedPhysicalRow::new(
        schema,
        PhysicalRow::from_values(values),
    ))
}

fn returning_image_values(
    image: Option<ReturningRowImage<'_>>,
    columns: &[String],
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Result<Vec<Value>, SQLError> {
    let Some(image) = image else {
        return Ok(vec![Value::Null; columns.len()]);
    };
    let mut document = image.document.clone();
    crate::engine_generated::materialize_virtual_generated_columns(definitions, &mut document)?;
    columns
        .iter()
        .map(|column| {
            if column == DOC_ID_COLUMN
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id_value(image.doc_id)
            } else {
                Ok(document.get(column).cloned().unwrap_or(Value::Null))
            }
        })
        .collect()
}

fn returning_context_schema(
    columns: &[String],
    types: &[Option<uqa_sql::ast::ColumnType>],
    target_qualifier: &str,
    aliases: &ReturningAliases,
) -> RowSchema {
    let target =
        RowSchema::with_qualified_types(target_qualifier, columns.to_vec(), types.to_vec());
    let hidden = ["old", "new"]
        .into_iter()
        .flat_map(|image| {
            types.iter().enumerate().map(move |(position, ty)| {
                (format!("\0uqa.returning.{image}.{position}"), ty.clone())
            })
        })
        .collect::<Vec<_>>();
    let schema = RowSchema::append_typed(&target, &hidden);
    let width = columns.len();
    let identity_aliases = columns
        .iter()
        .enumerate()
        .flat_map(|(position, column)| {
            [
                (
                    ColumnIdentity::qualified(&aliases.old, column),
                    width + position,
                ),
                (
                    ColumnIdentity::qualified(&aliases.new, column),
                    width * 2 + position,
                ),
            ]
        })
        .collect::<Vec<_>>();
    RowSchema::with_identity_aliases(&schema, &identity_aliases)
}

pub(in crate::sql) struct ReturningProjectionRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub images: ReturningRowImages<'a>,
    pub aliases: &'a ReturningAliases,
    pub context: Option<&'a OwnedPhysicalRow>,
}

pub(in crate::sql) fn build_returning_row(
    engine: &Engine,
    input: ReturningProjectionRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<OwnedPhysicalRow, SQLError> {
    let target = returning_row_context(
        engine,
        input.table,
        input.target_qualifier,
        input.images,
        input.aliases,
    )?;
    let row = input.context.map_or(target.clone(), |context| {
        OwnedPhysicalRow::new(
            RowSchema::join(&target.schema, &context.schema, std::iter::empty()),
            PhysicalRow::concat(&target.row, &context.row),
        )
    });
    let projections = expanded_returning_projections(
        engine,
        input.table,
        input.target_qualifier,
        input.aliases,
        returning,
    )?;
    let doc_id = input
        .images
        .new
        .or(input.images.old)
        .map_or(0, |image| image.doc_id);
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, ctes).map_err(
        |err| {
            SQLError::Internal(format!(
                "RETURNING projection failed for table `{}` doc {doc_id}: {err}",
                input.table
            ))
        },
    )
}

pub(in crate::sql) struct DmlReturningShape<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub aliases: &'a ReturningAliases,
    pub returning: &'a [ProjectionPlan],
    pub params: &'a [SQLParam],
    pub ctes: &'a CteScope,
    pub supplemental_schema: Option<&'a RowSchema>,
}

pub(in crate::sql) fn dml_returning_result(
    engine: &Engine,
    shape: DmlReturningShape<'_>,
    rows: Vec<OwnedPhysicalRow>,
    affected_rows: u64,
) -> Result<SQLResult, SQLError> {
    let star_schema = returning_target_schema(engine, shape.table)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        shape.target_qualifier,
        shape.aliases,
        shape.supplemental_schema,
    );
    let projections = expanded_returning_projections(
        engine,
        shape.table,
        shape.target_qualifier,
        shape.aliases,
        shape.returning,
    )?;
    let output = bind_projection_output_schema(
        engine,
        &projections,
        &expression_schema,
        &star_schema,
        &shape.ctes.scalar_subqueries,
        shape.params,
        shape.ctes,
    )?;
    let preserve_positions =
        output.columns().iter().collect::<BTreeSet<_>>().len() != output.columns().len();
    let mut named_rows = Vec::with_capacity(rows.len());
    let mut positional_rows = preserve_positions.then(|| Vec::with_capacity(rows.len()));
    for row in rows {
        if let Some(positional_rows) = positional_rows.as_mut() {
            positional_rows.push(row.view().iter().map(|(_, value)| value.clone()).collect());
        }
        named_rows.push(row.into_result_row());
    }
    let mut result = SQLResult::from_typed_rows_with_positions(
        output.columns().to_vec(),
        output.column_types().to_vec(),
        named_rows,
        positional_rows,
    );
    result.affected_rows = affected_rows;
    Ok(result)
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
    target_qualifier: &str,
    aliases: &ReturningAliases,
    supplemental: Option<&RowSchema>,
) -> RowSchema {
    let mut columns = target.columns().to_vec();
    let mut types = target.column_types().to_vec();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(uqa_sql::ast::ColumnType::BigInteger));
    }
    let target = returning_context_schema(&columns, &types, target_qualifier, aliases);
    supplemental.map_or(target.clone(), |source| {
        RowSchema::join(&target, source, std::iter::empty())
    })
}

pub(in crate::sql) fn expanded_returning_projections(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    aliases: &ReturningAliases,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let columns = returning_target_schema(engine, table)?.columns().to_vec();
    let mut projections = Vec::with_capacity(returning.len().max(columns.len()));
    for projection in returning {
        match &projection.expr {
            uqa_execution::ScalarExpr::Star => {
                projections.extend(columns.iter().map(|column| ProjectionPlan {
                    expr: uqa_execution::ScalarExpr::Column(column.clone()),
                    alias: Some(column.clone()),
                }));
            }
            uqa_execution::ScalarExpr::QualifiedStar(qualifier)
                if qualifier == target_qualifier
                    || qualifier == &aliases.old
                    || qualifier == &aliases.new =>
            {
                projections.extend(columns.iter().map(|column| ProjectionPlan {
                    expr: uqa_execution::ScalarExpr::QualifiedColumn {
                        qualifier: qualifier.clone(),
                        column: column.clone(),
                    },
                    alias: Some(column.clone()),
                }));
            }
            _ => projections.push(projection.clone()),
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
