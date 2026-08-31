//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE `RETURNING` row construction and projection.

use super::{
    build_projection_physical_row_with_ctes, dml_returning_result_with_projections,
    expanded_returning_projections, returning_row_context, returning_value_context, CteScope,
    DmlReturningShape, Engine, MergePlan, MutationRowImages, ProjectionPlan,
    ReturningValueProjectionRow, SQLError, SQLParam, SQLResult, Value,
};

#[derive(Clone)]
pub(super) struct MergeReturningRow<'a> {
    pub(super) target_table: &'a str,
    pub(super) target_qual: &'a str,
    pub(super) images: MutationRowImages<'a>,
    pub(super) returning_aliases: &'a uqa_sql::ast::ReturningAliases,
    pub(super) source_row: &'a uqa_execution::OwnedPhysicalRow,
    pub(super) source_schema: &'a uqa_execution::RowSchema,
    pub(super) source_relation: uqa_sql::ast::InternalRelationId,
    pub(super) action: &'a str,
}

pub(in crate::sql) struct ViewMergeReturningRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub current: &'a [Value],
    pub old: Option<&'a [Value]>,
    pub new: Option<&'a [Value]>,
    pub returning_aliases: &'a uqa_sql::ast::ReturningAliases,
    pub source_row: &'a uqa_execution::OwnedPhysicalRow,
    pub source_schema: &'a uqa_execution::RowSchema,
    pub source_relation: uqa_sql::ast::InternalRelationId,
    pub action: &'a str,
}

pub(in crate::sql) struct ViewMergeReturningResult<'a> {
    pub stmt: &'a MergePlan,
    pub source_schema: &'a uqa_execution::RowSchema,
    pub source_relation: uqa_sql::ast::InternalRelationId,
    pub params: &'a [SQLParam],
    pub ctes: &'a CteScope,
    pub rows: Vec<uqa_execution::OwnedPhysicalRow>,
    pub affected: u64,
}

pub(super) fn build_merge_returning_row(
    engine: &Engine,
    input: MergeReturningRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    let row = merge_returning_context(engine, input.clone())?;
    let projections = expanded_merge_returning_projections(
        engine,
        input.target_table,
        input.target_qual,
        input.returning_aliases,
        input.source_schema,
        input.source_relation,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, &snapshot_scope)
}

fn merge_returning_context(
    engine: &Engine,
    input: MergeReturningRow<'_>,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    let row = returning_row_context(
        engine,
        input.target_table,
        input.target_qual,
        input.images,
        input.returning_aliases,
    )?;
    append_merge_returning_metadata(
        row,
        input.source_row,
        input.source_schema,
        input.source_relation,
        input.action,
    )
}

fn append_merge_returning_metadata(
    mut row: uqa_execution::OwnedPhysicalRow,
    source_row: &uqa_execution::OwnedPhysicalRow,
    source_schema: &uqa_execution::RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
    action: &str,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    row.schema = uqa_execution::RowSchema::append_internal_typed(
        &row.schema,
        &[(
            crate::sql::merge_action_attribute(),
            Some(uqa_sql::ast::ColumnType::Text),
        )],
    );
    row.row = row.row.append_values(vec![Value::Str(action.into())]);
    let aliases = source_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, _)| {
            let slot = source_row.schema.physical_slot(position).ok_or_else(|| {
                SQLError::Internal(format!(
                    "MERGE RETURNING source lost physical column {position}"
                ))
            })?;
            Ok((
                source_relation.column(position),
                slot,
                source_schema.column_type(position).cloned(),
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let source_schema =
        uqa_execution::RowSchema::with_physical_internal_aliases(&source_row.schema, &aliases);
    row = uqa_execution::OwnedPhysicalRow::new(
        uqa_execution::RowSchema::join(&row.schema, &source_schema, std::iter::empty()),
        uqa_execution::PhysicalRow::concat(&row.row, &source_row.row),
    );
    Ok(row)
}

pub(in crate::sql) fn build_view_merge_returning_row(
    engine: &Engine,
    input: ViewMergeReturningRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    let target = returning_value_context(
        engine,
        ReturningValueProjectionRow {
            table: input.table,
            target_qualifier: input.target_qualifier,
            current: input.current,
            old: input.old,
            new: input.new,
            aliases: input.returning_aliases,
            context: None,
        },
    )?;
    let row = append_merge_returning_metadata(
        target,
        input.source_row,
        input.source_schema,
        input.source_relation,
        input.action,
    )?;
    let projections = expanded_merge_returning_projections(
        engine,
        input.table,
        input.target_qualifier,
        input.returning_aliases,
        input.source_schema,
        input.source_relation,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(engine, &row, &projections, params, &snapshot_scope)
}

pub(in crate::sql) fn finish_view_merge_returning(
    engine: &Engine,
    input: ViewMergeReturningResult<'_>,
) -> Result<SQLResult, SQLError> {
    let ViewMergeReturningResult {
        stmt,
        source_schema,
        source_relation,
        params,
        ctes,
        rows,
        affected,
    } = input;
    if stmt.returning.is_empty() {
        return Ok(SQLResult::from_affected(affected));
    }
    let projections = expanded_merge_returning_projections(
        engine,
        &stmt.target,
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        source_schema,
        source_relation,
        &stmt.returning,
    )?;
    let returning_source_schema = merge_returning_source_schema(source_schema, source_relation);
    dml_returning_result_with_projections(
        engine,
        DmlReturningShape {
            table: &stmt.target,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes,
            supplemental_schema: Some(&returning_source_schema),
        },
        &projections,
        rows,
        affected,
    )
}

pub(super) fn merge_returning_source_schema(
    source_schema: &uqa_execution::RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
) -> uqa_execution::RowSchema {
    let aliases = source_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, _)| {
            (
                source_relation.column(position),
                source_schema
                    .physical_slot(position)
                    .expect("source column has a physical slot"),
                source_schema.column_type(position).cloned(),
            )
        })
        .collect::<Vec<_>>();
    uqa_execution::RowSchema::with_physical_internal_aliases(source_schema, &aliases)
}

pub(super) fn expanded_merge_returning_projections(
    engine: &Engine,
    target_table: &str,
    target_qualifier: &str,
    aliases: &uqa_sql::ast::ReturningAliases,
    source_schema: &uqa_execution::RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let target_star = ProjectionPlan {
        expr: uqa_execution::ScalarExpr::QualifiedStar(target_qualifier.into()),
        alias: None,
    };
    let target_projections = expanded_returning_projections(
        engine,
        target_table,
        target_qualifier,
        aliases,
        std::slice::from_ref(&target_star),
    )?;
    let mut projections = Vec::new();
    for projection in returning {
        match &projection.expr {
            uqa_execution::ScalarExpr::Star => {
                projections.extend(
                    source_schema
                        .columns()
                        .iter()
                        .enumerate()
                        .filter(|(position, _)| {
                            crate::sql::select::visible_projection_source_position(
                                source_schema,
                                *position,
                            )
                        })
                        .map(|(position, column)| ProjectionPlan {
                            expr: uqa_execution::ScalarExpr::InternalColumn(
                                source_relation.column(position),
                            ),
                            alias: Some(
                                source_schema
                                    .public_name(position)
                                    .unwrap_or(column)
                                    .to_string(),
                            ),
                        }),
                );
                projections.extend(target_projections.iter().cloned());
            }
            uqa_execution::ScalarExpr::QualifiedStar(qualifier)
                if qualifier == target_qualifier
                    || qualifier == &aliases.old
                    || qualifier == &aliases.new =>
            {
                projections.extend(expanded_returning_projections(
                    engine,
                    target_table,
                    target_qualifier,
                    aliases,
                    std::slice::from_ref(projection),
                )?);
            }
            _ => projections.push(projection.clone()),
        }
    }
    Ok(projections)
}
