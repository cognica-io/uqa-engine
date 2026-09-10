//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute view mutation rows, rewrite rules, and INSTEAD OF triggers.
use crate::mutation::{
    assignment::MutationAssignmentContext,
    expressions::eval_mutation_expr,
    returning::{
        build_returning_value_row, dml_returning_result, DmlReturningShape,
        ReturningValueProjectionRow,
    },
    rows::join_rows as dml_join_rows,
};
use crate::query::{
    sources::build_join_spill_with_ctes,
    statement::context::{with_statement_snapshot, StatementContext},
    CteScope,
};
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use std::collections::BTreeSet;
use uqa_core::Value;
pub use uqa_sql::semantics::view_mutation::{
    coerce_view_value, target_columns, ViewMutationTarget as ViewDmlTarget,
};
use uqa_sql::semantics::view_mutation::{
    required_view_delete_columns, required_view_update_columns, resolve_view_target,
    view_qualification_references_target,
};
use uqa_sql::semantics::{
    mutation_qualifiers::validate_dml_expression_qualifiers,
    returning::validate_returning_alias_relations,
};
use uqa_sql::{
    plan::{DeletePlan, InsertPlan, QueryPlan, UpdatePlan},
    SQLError, SQLParam, SQLResult, ScalarExpr,
};
use uqa_storage::document_store::Document;

pub type SourceOutputPruning = fn(&mut QueryPlan, &BTreeSet<usize>, usize);
mod insert;
mod update_delete;
pub use insert::run_view_insert_inner;
pub use update_delete::{run_view_delete_inner, run_view_update_inner};

pub fn materialize_view_rows<S: Clone + Send + Sync + 'static>(
    context: &StatementContext<'_, S>,
    prune_source_outputs: SourceOutputPruning,
    target: &ViewDmlTarget,
    required_columns: Option<&BTreeSet<String>>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
) -> Result<Vec<Vec<Value>>, SQLError> {
    let mut query = target.definition.query.clone();
    if let Some(required_columns) = required_columns {
        let required_positions = target
            .columns
            .iter()
            .enumerate()
            .filter_map(|(position, column)| required_columns.contains(column).then_some(position))
            .collect::<BTreeSet<_>>();
        prune_source_outputs(&mut query, &required_positions, target.columns.len());
    }
    let privilege_subject = if target.definition.security_invoker() {
        scope.privilege_subject()?.to_string()
    } else {
        target.definition.role_owner.clone()
    };
    let mut privilege_scope = scope.enter_privilege_subject(privilege_subject);
    let result = crate::query::statement::execute_query_plan_with_ctes(
        context,
        &query,
        params,
        &mut privilege_scope,
    )?;
    if result.columns.len() != target.columns.len() {
        return Err(SQLError::Internal(format!(
            "view `{}` returned {} columns for a {}-column row type",
            target.canonical_name,
            result.columns.len(),
            target.columns.len()
        )));
    }
    (0..result.rows.len())
        .map(|row| {
            (0..target.columns.len())
                .map(|column| {
                    result.value_at(row, column).cloned().ok_or_else(|| {
                        SQLError::Internal(format!(
                            "view `{}` omitted result column {}",
                            target.canonical_name,
                            column + 1
                        ))
                    })
                })
                .collect()
        })
        .collect()
}

pub fn target_row(
    target: &ViewDmlTarget,
    qualifier: &str,
    values: &[Value],
) -> Result<OwnedPhysicalRow, SQLError> {
    if values.len() != target.columns.len() {
        return Err(SQLError::Internal(
            "view DML row does not match its declared row type".into(),
        ));
    }
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, target.columns.clone(), target.types.clone()),
        PhysicalRow::from_values(values.to_vec()),
    ))
}

fn values_from_result(result: SQLResult) -> Result<Vec<Vec<Value>>, SQLError> {
    (0..result.rows.len())
        .map(|row| {
            (0..result.columns.len())
                .map(|column| {
                    result.value_at(row, column).cloned().ok_or_else(|| {
                        SQLError::Internal(format!(
                            "query result omitted output column {}",
                            column + 1
                        ))
                    })
                })
                .collect()
        })
        .collect()
}

fn view_document(target: &ViewDmlTarget, values: &[Value]) -> Result<Document, SQLError> {
    if values.len() != target.columns.len() {
        return Err(SQLError::Internal(
            "view rule row does not match its declared row type".into(),
        ));
    }
    Ok(target
        .columns
        .iter()
        .cloned()
        .zip(values.iter().cloned())
        .collect())
}

fn cached_view_document(
    target: &ViewDmlTarget,
    values: &[Option<Value>],
) -> Result<Document, SQLError> {
    if values.len() != target.columns.len() {
        return Err(SQLError::Internal(
            "cached view rule row does not match its declared row type".into(),
        ));
    }
    Ok(target
        .columns
        .iter()
        .cloned()
        .zip(values)
        .filter_map(|(column, value)| value.clone().map(|value| (column, value)))
        .collect())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
fn evaluate_insert_rule_column<S: Clone + Send + Sync + 'static>(
    assignment: &MutationAssignmentContext<'_, S>,
    target: &ViewDmlTarget,
    positions: &[usize],
    expressions: &[ScalarExpr],
    column: &str,
    values: &mut [Option<Value>],
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<Value, SQLError> {
    let target_position = target
        .columns
        .iter()
        .position(|candidate| candidate == column)
        .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?;
    if let Some(value) = values[target_position].as_ref() {
        return Ok(value.clone());
    }
    let value = if let Some(input_position) = positions
        .iter()
        .position(|position| *position == target_position)
    {
        let expression = expressions.get(input_position).ok_or_else(|| {
            SQLError::Internal("view rule INSERT input lost its expression".into())
        })?;
        if matches!(expression, ScalarExpr::Default) {
            Value::Null
        } else {
            eval_mutation_expr(assignment.expressions, scope, expression, None, params)?
        }
    } else {
        Value::Null
    };
    let value = coerce_view_value(assignment.assignment, target, target_position, value)?;
    values[target_position] = Some(value.clone());
    Ok(value)
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
fn evaluate_insert_rule_columns<S: Clone + Send + Sync + 'static>(
    assignment: &MutationAssignmentContext<'_, S>,
    target: &ViewDmlTarget,
    positions: &[usize],
    expressions: &[ScalarExpr],
    required: &BTreeSet<String>,
    values: &mut [Option<Value>],
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<(), SQLError> {
    for column in required {
        let _ = evaluate_insert_rule_column(
            assignment,
            target,
            positions,
            expressions,
            column,
            values,
            params,
            scope,
        )?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
fn run_suppressed_view_insert_rules<S: Clone + Send + Sync + 'static>(
    context: &StatementContext<'_, S>,
    read_assignment: &MutationAssignmentContext<'_, S>,
    stmt: &InsertPlan,
    target: &ViewDmlTarget,
    positions: &[usize],
    columns: &[String],
    implicit_columns: bool,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<SQLResult, SQLError> {
    let snapshot = ctes.returning_statement_snapshot_scope();
    let mut cached_rows = Vec::with_capacity(stmt.rows.len());
    for expressions in &stmt.rows {
        if expressions.len() > columns.len()
            || (!implicit_columns && expressions.len() != columns.len())
        {
            return Err(SQLError::TypeMismatch(format!(
                "row width {} != column count {}",
                expressions.len(),
                columns.len()
            )));
        }
        let values = vec![None; target.columns.len()];
        cached_rows.push(values);
    }
    let rule_rows = cached_rows
        .iter()
        .map(|values| {
            Ok(crate::mutation::rules::RuleRowImage {
                old_storage_table: None,
                old_doc_id: None,
                old: None,
                new_storage_table: None,
                new_doc_id: None,
                new: Some(cached_view_document(target, values)?),
                context: None,
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut rule_batch = crate::mutation::rules::prepare_rule_batch_with_projection(
        context.mutation.rules.rules,
        &target.canonical_name,
        uqa_sql::ast::RuleEvent::Insert,
        rule_rows,
        |row_index, side, column| {
            if matches!(side, crate::mutation::rules::RuleRowSide::Old) {
                return Ok(None);
            }
            let expressions = stmt
                .rows
                .get(row_index)
                .ok_or_else(|| SQLError::Internal("view rule INSERT lost its input row".into()))?;
            let values = cached_rows
                .get_mut(row_index)
                .ok_or_else(|| SQLError::Internal("view rule INSERT lost its cached row".into()))?;
            evaluate_insert_rule_column(
                read_assignment,
                target,
                positions,
                expressions,
                column,
                values,
                params,
                &snapshot,
            )
            .map(Some)
        },
    )?;
    let action_columns = rule_batch.missing_action_row_columns();
    for ((expressions, values), (_, required)) in
        stmt.rows.iter().zip(&mut cached_rows).zip(&action_columns)
    {
        evaluate_insert_rule_columns(
            read_assignment,
            target,
            positions,
            expressions,
            required,
            values,
            params,
            &snapshot,
        )?;
    }
    rule_batch.supplement_rows(
        cached_rows
            .iter()
            .map(|values| {
                Ok(crate::mutation::rules::RuleRowImage {
                    old_storage_table: None,
                    old_doc_id: None,
                    old: None,
                    new_storage_table: None,
                    new_doc_id: None,
                    new: Some(cached_view_document(target, values)?),
                    context: None,
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?,
    )?;
    let outcome = rule_batch.execute_actions_with_affected(
        context.mutation.rules.rules,
        crate::mutation::rules::RuleReturningRequest::from_plan(
            &stmt.returning,
            &stmt.returning_aliases,
            &stmt.subqueries,
        ),
    )?;
    if let Some(returning) = outcome.returning {
        return returning.project(
            context.mutation.preparation.returning,
            DmlReturningShape {
                table: &target.canonical_name,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes,
                supplemental_schema: None,
            },
        );
    }
    finish_view_dml(
        &context.mutation.preparation.returning,
        DmlReturningShape {
            table: &target.canonical_name,
            target_qualifier: &stmt.target_qualifier,
            aliases: &stmt.returning_aliases,
            returning: &stmt.returning,
            params,
            ctes,
            supplemental_schema: None,
        },
        Vec::new(),
        outcome.affected_rows,
    )
}

fn finish_view_dml<S: Clone + 'static>(
    returning: &crate::mutation::returning::ReturningExecutionContext<'_, S>,
    shape: DmlReturningShape<'_, S>,
    returning_rows: Vec<OwnedPhysicalRow>,
    affected: u64,
) -> Result<SQLResult, SQLError> {
    if shape.returning.is_empty() {
        return Ok(SQLResult::from_affected(affected));
    }
    dml_returning_result(*returning, shape, returning_rows, affected)
}
