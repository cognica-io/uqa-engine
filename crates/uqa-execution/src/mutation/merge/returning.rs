//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE `RETURNING` row construction and projection.

use crate::mutation::{
    returning::{
        dml_returning_result_with_projections, returning_row_context, returning_value_context,
        DmlReturningShape, ReturningExecutionContext, ReturningValueProjectionRow,
    },
    row_images::MutationRowImages,
};
use crate::query::{relational::project_row::build_projection_physical_row_with_ctes, CteScope};
use uqa_core::Value;
use uqa_sql::semantics::merge::{
    expanded_merge_returning_projections, merge_returning_source_schema,
};
use uqa_sql::{
    plan::{MergePlan, ProjectionPlan},
    SQLError, SQLParam, SQLResult,
};

#[derive(Clone)]
pub struct MergeReturningRow<'a> {
    pub target_table: &'a str,
    pub target_qual: &'a str,
    pub images: MutationRowImages<'a>,
    pub returning_aliases: &'a uqa_sql::ast::ReturningAliases,
    pub source_row: &'a crate::OwnedPhysicalRow,
    pub source_schema: &'a crate::RowSchema,
    pub source_relation: uqa_sql::ast::InternalRelationId,
    pub action: &'a str,
}

pub struct ViewMergeReturningRow<'a> {
    pub table: &'a str,
    pub target_qualifier: &'a str,
    pub current: &'a [Value],
    pub old: Option<&'a [Value]>,
    pub new: Option<&'a [Value]>,
    pub returning_aliases: &'a uqa_sql::ast::ReturningAliases,
    pub source_row: &'a crate::OwnedPhysicalRow,
    pub source_schema: &'a crate::RowSchema,
    pub source_relation: uqa_sql::ast::InternalRelationId,
    pub action: &'a str,
}

pub struct ViewMergeReturningResult<'a, S: Clone + 'static> {
    pub stmt: &'a MergePlan,
    pub source_schema: &'a crate::RowSchema,
    pub source_relation: uqa_sql::ast::InternalRelationId,
    pub params: &'a [SQLParam],
    pub ctes: &'a CteScope<S>,
    pub rows: Vec<crate::OwnedPhysicalRow>,
    pub affected: u64,
}

pub fn build_merge_returning_row<S: Clone + 'static>(
    context: &ReturningExecutionContext<'_, S>,
    input: MergeReturningRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<crate::OwnedPhysicalRow, SQLError> {
    let row = merge_returning_context(context, input.clone())?;
    let projections = expanded_merge_returning_projections(
        context.catalog,
        input.target_table,
        input.target_qual,
        input.returning_aliases,
        input.source_schema,
        input.source_relation,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(
        context.relational,
        &row,
        &projections,
        params,
        &snapshot_scope,
    )
}

fn merge_returning_context<S: Clone + 'static>(
    context: &ReturningExecutionContext<'_, S>,
    input: MergeReturningRow<'_>,
) -> Result<crate::OwnedPhysicalRow, SQLError> {
    let row = returning_row_context(
        *context,
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
    mut row: crate::OwnedPhysicalRow,
    source_row: &crate::OwnedPhysicalRow,
    source_schema: &crate::RowSchema,
    source_relation: uqa_sql::ast::InternalRelationId,
    action: &str,
) -> Result<crate::OwnedPhysicalRow, SQLError> {
    row.schema = crate::RowSchema::append_internal_typed(
        &row.schema,
        &[(
            uqa_sql::semantics::merge_action_attribute(),
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
        crate::RowSchema::with_physical_internal_aliases(&source_row.schema, &aliases);
    row = crate::OwnedPhysicalRow::new(
        crate::RowSchema::join(&row.schema, &source_schema, std::iter::empty()),
        crate::PhysicalRow::concat(&row.row, &source_row.row),
    );
    Ok(row)
}

pub fn build_view_merge_returning_row<S: Clone + 'static>(
    context: &ReturningExecutionContext<'_, S>,
    input: ViewMergeReturningRow<'_>,
    returning: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<crate::OwnedPhysicalRow, SQLError> {
    let target = returning_value_context(
        *context,
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
        context.catalog,
        input.table,
        input.target_qualifier,
        input.returning_aliases,
        input.source_schema,
        input.source_relation,
        returning,
    )?;
    let snapshot_scope = ctes.returning_statement_snapshot_scope();
    build_projection_physical_row_with_ctes(
        context.relational,
        &row,
        &projections,
        params,
        &snapshot_scope,
    )
}

pub fn finish_view_merge_returning<S: Clone + 'static>(
    context: &ReturningExecutionContext<'_, S>,
    input: ViewMergeReturningResult<'_, S>,
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
        context.catalog,
        &stmt.target,
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        source_schema,
        source_relation,
        &stmt.returning,
    )?;
    let returning_source_schema = merge_returning_source_schema(source_schema, source_relation);
    dml_returning_result_with_projections(
        *context,
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
