//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Point UPDATE qualification, locking, and storage patch execution.
use super::{
    candidate::MutationLockTarget, errors::dml_storage_error, expressions::eval_mutation_expr,
    locking::lock_mutation_target,
};
use crate::query::locking::context::update_lock_strength;
use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_sql::{
    assignment::columns::coerce_to_column_type,
    ast::{BinaryOp, ColumnType},
    plan::UpdatePlan,
    semantics::{
        mutation_inputs::{expr_is_row_independent, top_level_column},
        mutation_patch as patch_eligibility,
    },
    SQLError, SQLParam, SQLResult, ScalarExpr,
};
pub mod context;
pub use context::{PointMutationContext, RowIndependentUpdateValues, RowUpdateVectors};

pub fn try_run_point_update<S: Clone + 'static>(
    context: PointMutationContext<'_, S>,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<SQLResult>, SQLError> {
    if context
        .constraints
        .catalog
        .try_describe_table(&stmt.table)
        .map_err(|error| dml_storage_error("UPDATE", error))?
        .is_some_and(|columns| columns.iter().any(|column| column.generated.is_some()))
    {
        return Ok(None);
    }
    if !stmt.returning.is_empty() {
        return Ok(None);
    }
    let Some((lookup_field, lookup_value)) = point_lookup_filter(
        stmt.predicate.as_ref(),
        context,
        params,
        stmt.statement_privilege_subject.as_deref(),
        stmt.relations_bound,
    )?
    else {
        return Ok(None);
    };
    let Some((updates, vectors)) = row_independent_update_values(context, stmt, params)? else {
        return Ok(None);
    };
    if !patch_eligibility::can_patch_update_without_full_row(
        context.constraints.catalog,
        &stmt.table,
        context.constraints.referrers,
        &updates,
    )? {
        return Ok(None);
    }
    if matches!(lookup_value, Value::Null) {
        return Ok(Some(SQLResult::from_affected(0)));
    }
    if !patch_eligibility::point_lookup_field_is_unique(
        context.constraints.catalog,
        &stmt.table,
        &lookup_field,
    )? {
        return Ok(None);
    }
    let Some(doc_id) =
        context
            .storage
            .find_doc_id_by_field(&stmt.table, &lookup_field, &lookup_value)?
    else {
        return Ok(Some(SQLResult::from_affected(0)));
    };
    let target = lock_mutation_target(
        context.locking.session,
        &stmt.table,
        &stmt.target_qualifier,
        doc_id,
        update_lock_strength(
            context.locking.catalog,
            &stmt.table,
            &stmt
                .assignments
                .iter()
                .map(|assignment| assignment.column.clone())
                .collect::<Vec<_>>(),
        ),
    )?;
    let MutationLockTarget::Present { doc_id, .. } = target else {
        return Ok(Some(SQLResult::from_affected(0)));
    };
    context.transaction.prepare_writer()?;
    if context
        .storage
        .find_doc_id_by_field(&stmt.table, &lookup_field, &lookup_value)?
        != Some(doc_id)
    {
        return Ok(Some(SQLResult::from_affected(0)));
    }
    let affected = context.storage.patch_document_fields_with_vector_values(
        &stmt.table,
        doc_id,
        &updates,
        &vectors,
    )?;
    Ok(Some(SQLResult::from_affected(u64::from(affected))))
}

pub fn point_lookup_filter<S: Clone + 'static>(
    filter: Option<&ScalarExpr>,
    context: PointMutationContext<'_, S>,
    params: &[SQLParam],
    privilege_subject: Option<&str>,
    relations_bound: bool,
) -> Result<Option<(String, Value)>, SQLError> {
    let Some(ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    }) = filter
    else {
        return Ok(None);
    };
    if let Some(field) = top_level_column(lhs) {
        if expr_is_row_independent(rhs) {
            let ctes = context
                .scopes
                .command_scope(privilege_subject, relations_bound)?;
            return Ok(Some((
                field.to_string(),
                eval_mutation_expr(context.assignment.expressions, &ctes, rhs, None, params)?,
            )));
        }
    }
    if let Some(field) = top_level_column(rhs) {
        if expr_is_row_independent(lhs) {
            let ctes = context
                .scopes
                .command_scope(privilege_subject, relations_bound)?;
            return Ok(Some((
                field.to_string(),
                eval_mutation_expr(context.assignment.expressions, &ctes, lhs, None, params)?,
            )));
        }
    }
    Ok(None)
}

pub fn row_independent_update_values<S: Clone + 'static>(
    context: PointMutationContext<'_, S>,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<RowIndependentUpdateValues>, SQLError> {
    let mut updates = BTreeMap::new();
    let mut vectors = BTreeMap::new();
    let ctes = context.scopes.command_scope(
        stmt.statement_privilege_subject.as_deref(),
        stmt.relations_bound,
    )?;
    for assignment in &stmt.assignments {
        if !expr_is_row_independent(&assignment.value) {
            return Ok(None);
        }
        let value = coerce_to_column_type(
            context.assignment.assignment,
            context.assignment.columns,
            &stmt.table,
            &assignment.column,
            eval_mutation_expr(
                context.assignment.expressions,
                &ctes,
                &assignment.value,
                None,
                params,
            )?,
        )?;
        if let Some(ty @ (ColumnType::Vector(_) | ColumnType::Tensor(_))) = context
            .constraints
            .catalog
            .column_type(&stmt.table, &assignment.column)
            .map_err(|err| dml_storage_error("UPDATE", err))?
        {
            let values = index_vectors_for_type(&value, &ty)?;
            vectors.insert(assignment.column.clone(), values);
        }
        updates.insert(assignment.column.clone(), value);
    }
    Ok(Some((updates, vectors)))
}

use uqa_sql::assignment::vectors::index_vectors_for_type;
