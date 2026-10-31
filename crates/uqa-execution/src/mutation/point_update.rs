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
use crate::mutation::output::MutationOutput;
use crate::query::locking::context::update_lock_strength;
use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_sql::{
    assignment::columns::coerce_to_column_type_from,
    ast::ColumnType,
    plan::UpdatePlan,
    semantics::{
        mutation_inputs::{
            point_lookup_input, row_independent_assignments, RowIndependentAssignment,
        },
        mutation_patch as patch_eligibility,
    },
    SQLError, SQLParam,
};
pub mod context;
pub use context::{PointMutationContext, RowIndependentUpdateValues, RowUpdateVectors};

struct PointUpdateInput<'a> {
    lookup_field: &'a str,
    lookup_input: &'a uqa_sql::ScalarExpr,
    assignments: Vec<RowIndependentAssignment<'a>>,
}

impl<'a> PointUpdateInput<'a> {
    /// Complete physical path selection before any lookup expression or assignment can run.
    fn select<S: Clone + 'static>(
        context: &PointMutationContext<'_, S>,
        stmt: &'a UpdatePlan,
        columns: Option<&[uqa_sql::ast::ColumnDef]>,
    ) -> Result<Option<Self>, SQLError> {
        if !stmt.returning.is_empty()
            || columns
                .is_some_and(|columns| columns.iter().any(|column| column.generated.is_some()))
        {
            return Ok(None);
        }
        let Some((lookup_field, lookup_input)) = point_lookup_input(stmt.predicate.as_ref()) else {
            return Ok(None);
        };
        let Some(assignments) = row_independent_assignments(&stmt.assignments) else {
            return Ok(None);
        };
        if !patch_eligibility::can_patch_update_without_full_row(
            context.constraints.catalog,
            &stmt.table,
            context.constraints.referrers,
            assignments.iter().map(|assignment| assignment.column),
        )? || !patch_eligibility::point_lookup_field_is_unique(
            context.constraints.catalog,
            &stmt.table,
            lookup_field,
        )? {
            return Ok(None);
        }
        Ok(Some(Self {
            lookup_field,
            lookup_input,
            assignments,
        }))
    }
}

pub fn try_run_point_update<S: Clone + 'static>(
    context: PointMutationContext<'_, S>,
    stmt: &UpdatePlan,
    params: &[SQLParam],
) -> Result<Option<MutationOutput>, SQLError> {
    let columns = context
        .constraints
        .catalog
        .try_describe_table(&stmt.table)
        .map_err(|error| dml_storage_error("UPDATE", error))?;
    let Some(PointUpdateInput {
        lookup_field,
        lookup_input,
        assignments,
    }) = PointUpdateInput::select(&context, stmt, columns.as_deref())?
    else {
        return Ok(None);
    };
    let scope = context.scopes.command_scope(
        stmt.statement_privilege_subject.as_ref(),
        stmt.relations_bound,
    )?;
    let lookup_value = eval_mutation_expr(
        context.assignment.expressions,
        &scope,
        lookup_input,
        None,
        params,
    )?;
    if matches!(lookup_value, Value::Null) {
        return Ok(Some(MutationOutput::from_affected(0)));
    }
    if let Some(read) = crate::serializable::SerializableRelationRead::for_mutation(
        context.observations,
        &stmt.table,
    )? {
        read.observe_column_index(
            columns
                .as_deref()
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?,
            &uqa_storage::ValueIndexKey::Column(lookup_field.to_owned()),
            &uqa_core::Predicate::Equals(lookup_value.clone()),
        )?;
    }
    let Some(doc_id) =
        context
            .storage
            .find_doc_id_by_field(&stmt.table, lookup_field, &lookup_value)?
    else {
        return Ok(Some(MutationOutput::from_affected(0)));
    };
    let (mut updates, mut vectors) =
        evaluate_point_assignments(&context, stmt, &assignments, params)?;
    let assigned_columns = assignments
        .iter()
        .map(|assignment| assignment.column.to_owned())
        .collect::<Vec<_>>();
    let target = lock_mutation_target(
        context.locking.session,
        &stmt.table,
        &stmt.target_qualifier,
        doc_id,
        update_lock_strength(context.locking.catalog, &stmt.table, &assigned_columns),
    )?;
    let MutationLockTarget::Present { doc_id, recheck } = target else {
        return Ok(Some(MutationOutput::from_affected(0)));
    };
    if recheck {
        context
            .constraints
            .transactions
            .refresh_explicit_statement_snapshot()?;
    }
    context.transaction.prepare_writer()?;
    if context
        .storage
        .find_doc_id_by_field(&stmt.table, lookup_field, &lookup_value)?
        != Some(doc_id)
    {
        return Ok(Some(MutationOutput::from_affected(0)));
    }
    if recheck {
        (updates, vectors) = evaluate_point_assignments(&context, stmt, &assignments, params)?;
    }
    crate::mutation::constraints::validate_patch_not_null(
        context.constraints,
        &stmt.table,
        doc_id,
        columns.as_deref().unwrap_or_default(),
        &assigned_columns,
        &updates,
    )?;
    let affected = patch_point_row(&context, &stmt.table, doc_id, &updates, &vectors)?;
    Ok(Some(MutationOutput::from_affected(u64::from(affected))))
}

/// Write the patched fields of the row that `table` holds at `doc_id`, and note the row for the statements that started the running one.
fn patch_point_row<S: Clone + 'static>(
    context: &PointMutationContext<'_, S>,
    table: &str,
    doc_id: uqa_core::DocId,
    updates: &BTreeMap<String, Value>,
    vectors: &RowUpdateVectors,
) -> Result<bool, SQLError> {
    crate::serializable::observe_row_write(context.observations, table, doc_id)?;
    let affected = context
        .storage
        .patch_document_fields_with_vector_values(table, doc_id, updates, vectors)?;
    if affected {
        crate::mutation::statement_end::note_written_row(table, doc_id);
    }
    Ok(affected)
}

fn evaluate_point_assignments<S: Clone + 'static>(
    context: &PointMutationContext<'_, S>,
    stmt: &UpdatePlan,
    assignments: &[RowIndependentAssignment<'_>],
    params: &[SQLParam],
) -> Result<RowIndependentUpdateValues, SQLError> {
    let mut updates = BTreeMap::new();
    let mut vectors = BTreeMap::new();
    let ctes = context.scopes.command_scope(
        stmt.statement_privilege_subject.as_ref(),
        stmt.relations_bound,
    )?;
    let hook = context
        .assignment
        .expressions
        .expressions
        .bind_scope(&ctes)?;
    for assignment in assignments {
        let source = uqa_sql::type_resolution::assignment_source_type(
            assignment.value,
            &crate::RowSchema::default(),
            params,
            hook.as_ref(),
        )?;
        let value = coerce_to_column_type_from(
            context.assignment.assignment,
            context.assignment.columns,
            &stmt.table,
            assignment.column,
            eval_mutation_expr(
                context.assignment.expressions,
                &ctes,
                assignment.value,
                None,
                params,
            )?,
            source.as_ref(),
        )?;
        if let Some(ty @ (ColumnType::Vector(_) | ColumnType::Tensor(_))) = context
            .constraints
            .catalog
            .column_type(&stmt.table, assignment.column)
            .map_err(|err| dml_storage_error("UPDATE", err))?
        {
            let values = index_vectors_for_type(&value, &ty)?;
            vectors.insert(assignment.column.to_owned(), values);
        }
        updates.insert(assignment.column.to_owned(), value);
    }
    Ok((updates, vectors))
}

use uqa_sql::assignment::vectors::index_vectors_for_type;
