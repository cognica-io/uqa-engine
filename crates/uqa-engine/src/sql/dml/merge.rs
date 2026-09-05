//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE matching, action execution, and RETURNING projection.

use super::{
    apply_missing_column_defaults, build_join_spill_with_ctes,
    build_projection_physical_row_with_ctes, dml_join_rows, dml_null_target_row,
    dml_returning_result_with_projections, dml_storage_error, dml_target_row_for_storage,
    eval_mutation_assignment, eval_mutation_expr, expanded_returning_projections,
    finish_mutation_publication, insert_identity_columns, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies, lock_physical_mutation_target,
    missing_document_error, partition_insert_target, persist_auto_increment_identity,
    prepare_auto_increment_identity, prepare_document_delete, prepare_insert_identity,
    prepare_partition_update_route, prepare_routed_document_rewrite,
    refresh_insert_identity_after_trigger, returning_expression_schema, returning_row_context,
    returning_target_schema, returning_value_context, stage_prepared_document_delete,
    stage_prepared_document_rewrite, update_lock_strength, validate_document_constraints,
    validate_mutation_columns, validate_returning_alias_relations, validate_view_checks, BTreeMap,
    BTreeSet, CteScope, DmlReturningShape, Document, Engine, MergePlan, MergeWhenPlan,
    MutationAssignmentTarget, MutationOverlayScope, MutationPublicationBatch, MutationRowImage,
    MutationRowImages, PhysicalMutationLockTarget, PreparedDocumentInsert, PreparedMutationAction,
    ProjectionPlan, ReturningValueProjectionRow, SQLError, SQLParam, SQLResult, Value,
    ViewCheckContext,
};

mod codec;
mod returning;

use codec::{
    decode_merge_pair, decode_prepared_mutation_action_row, encode_merge_pair, merge_pair_schema,
    prepared_mutation_action_schema, push_prepared_mutation_action,
};
pub(in crate::sql) use codec::{merge_source_index_value, MergePairKind};
use returning::{
    build_merge_returning_row, expanded_merge_returning_projections, merge_returning_source_schema,
    MergeReturningRow,
};
pub(in crate::sql) use returning::{
    build_view_merge_returning_row, finish_view_merge_returning, ViewMergeReturningResult,
    ViewMergeReturningRow,
};

pub(in crate::sql) fn merge_command_returning_schema(
    engine: &Engine,
    stmt: &MergePlan,
    params: &[SQLParam],
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    if stmt.returning.is_empty() {
        return Ok(None);
    }
    let mut ctes = CteScope::new_for_statement(engine, stmt.statement_privilege_subject.as_deref());
    ctes.scalar_subqueries.clone_from(&stmt.subqueries);
    let source_schema =
        crate::sql::select::analyze_source_plan_schema(engine, &stmt.source, params, &ctes, None)?;
    validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        Some(&source_schema),
    )?;
    let target = dml_null_target_row(engine, &stmt.target, &stmt.target_qualifier)?;
    validate_merge_action_scopes(engine, stmt, &target.schema, &source_schema, params)?;
    let source_relation = uqa_sql::ast::InternalRelationId::allocate();
    let projections = expanded_merge_returning_projections(
        engine,
        &stmt.target,
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        &source_schema,
        source_relation,
        &stmt.returning,
    )?;
    let returning_source_schema = merge_returning_source_schema(&source_schema, source_relation);
    let star_schema = returning_target_schema(engine, &stmt.target)?;
    let expression_schema = returning_expression_schema(
        &star_schema,
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        Some(&returning_source_schema),
    );
    crate::sql::select::analyze_projection_output_schema(
        engine,
        &projections,
        &expression_schema,
        &star_schema,
        &stmt.subqueries,
        params,
        &ctes,
    )
    .map(Some)
}

mod execution;
use execution::{MergeTargetIdentity, SelectedMergeAction};

pub(in crate::sql) fn run_merge(
    engine: &Engine,
    mut stmt: MergePlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    stmt.target = super::resolve_dml_target_name(engine, &stmt.target, false)?;
    super::run_mutation_command(engine, move |engine| {
        execution::run_merge_inner(engine, &stmt, params)
    })
}

fn validate_view_merge_dispatch_contract(
    engine: &Engine,
    stmt: &MergePlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let mut scope =
        CteScope::new_for_statement(engine, stmt.statement_privilege_subject.as_deref());
    scope.scalar_subqueries.clone_from(&stmt.subqueries);
    let source =
        crate::sql::select::analyze_source_plan_schema(engine, &stmt.source, params, &scope, None)?;
    super::view_automatic::validate_public_merge_targets(engine, stmt)?;
    super::view_automatic::validate_public_merge_contract(engine, stmt, &source)
}

pub(in crate::sql) fn merge_privilege_expressions(
    stmt: &MergePlan,
) -> Vec<&uqa_execution::ScalarExpr> {
    let mut expressions = vec![&stmt.join_condition];
    expressions.extend(stmt.target_predicate.iter());
    expressions.extend(stmt.returning.iter().map(|projection| &projection.expr));
    for clause in &stmt.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
            | MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                expressions.extend(condition.iter());
                expressions.extend(assignments.iter().map(|assignment| &assignment.value));
            }
            MergeWhenPlan::InsertNotMatched {
                condition, values, ..
            } => {
                expressions.extend(condition.iter());
                expressions.extend(values);
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingMatched { condition }
            | MergeWhenPlan::NothingNotMatched { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                expressions.extend(condition.iter());
            }
        }
    }
    expressions
}

#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub(in crate::sql) fn validate_merge_action_scopes(
    engine: &Engine,
    stmt: &MergePlan,
    target_schema: &uqa_execution::RowSchema,
    source_schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let matched_schema =
        uqa_execution::RowSchema::join(target_schema, source_schema, std::iter::empty());
    let expression_type = |expression: &uqa_execution::ScalarExpr,
                           schema: &uqa_execution::RowSchema| {
        uqa_execution::scalar_type_with_resolver(expression, schema, params, engine)
    };
    let validate_boolean = |expression: &uqa_execution::ScalarExpr,
                            schema: &uqa_execution::RowSchema,
                            label: &str|
     -> Result<(), SQLError> {
        if expression_type(expression, schema)?
            .is_some_and(|ty| ty != uqa_sql::ast::ColumnType::Boolean)
        {
            return Err(SQLError::TypeMismatch(format!(
                "argument of {label} must be type boolean"
            )));
        }
        Ok(())
    };
    validate_boolean(&stmt.join_condition, &matched_schema, "MERGE ON")?;
    let has_source_missing = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                | MergeWhenPlan::NothingNotMatchedBySource { .. }
        )
    });
    let has_target_missing = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. }
        )
    });
    if has_source_missing
        && has_target_missing
        && !crate::sql::from_rows::join_conjuncts(&stmt.join_condition)
            .into_iter()
            .any(|conjunct| {
                matches!(
                    conjunct,
                    uqa_execution::ScalarExpr::Binary {
                        op: uqa_sql::ast::BinaryOp::Equal,
                        lhs,
                        rhs,
                    } if crate::sql::from_rows::decide_join_sides(
                        target_schema,
                        source_schema,
                        lhs,
                        rhs,
                    )
                    .is_some()
                )
            })
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message:
                "FULL JOIN is only supported with merge-joinable or hash-joinable join conditions"
                    .into(),
        });
    }
    for clause in &stmt.when_clauses {
        let (condition, expressions, schema): (
            Option<&uqa_execution::ScalarExpr>,
            Vec<&uqa_execution::ScalarExpr>,
            &uqa_execution::RowSchema,
        ) = match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => (
                condition.as_ref(),
                assignments
                    .iter()
                    .map(|assignment| &assignment.value)
                    .collect(),
                &matched_schema,
            ),
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                (condition.as_ref(), Vec::new(), &matched_schema)
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => (
                condition.as_ref(),
                assignments
                    .iter()
                    .map(|assignment| &assignment.value)
                    .collect(),
                target_schema,
            ),
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                (condition.as_ref(), Vec::new(), target_schema)
            }
            MergeWhenPlan::InsertNotMatched {
                condition, values, ..
            } => (condition.as_ref(), values.iter().collect(), source_schema),
            MergeWhenPlan::NothingNotMatched { condition } => {
                (condition.as_ref(), Vec::new(), source_schema)
            }
        };
        if let Some(condition) = condition {
            validate_boolean(condition, schema, "WHEN")?;
        }
        for expression in expressions {
            expression_type(expression, schema)?;
        }
    }
    Ok(())
}

fn ensure_merge_target_is_modified_once(
    mutated_target_ids: &mut BTreeSet<MergeTargetIdentity>,
    storage_table: &str,
    doc_id: uqa_core::DocId,
) -> Result<(), SQLError> {
    if mutated_target_ids.insert((storage_table.to_string(), doc_id)) {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "21000".into(),
        message: "MERGE command cannot affect row a second time".into(),
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
fn select_merge_action(
    engine: &Engine,
    stmt: &MergePlan,
    target_table: &str,
    match_kind: MergePairKind,
    doc_id: Option<uqa_core::DocId>,
    target_document: Option<&Document>,
    action_row: &uqa_execution::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SelectedMergeAction, SQLError> {
    for clause in &stmt.when_clauses {
        let (condition, applies) = match clause {
            MergeWhenPlan::UpdateMatched { condition, .. }
            | MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition }
                if matches!(match_kind, MergePairKind::Matched) =>
            {
                (condition.as_ref(), true)
            }
            MergeWhenPlan::InsertNotMatched { condition, .. }
            | MergeWhenPlan::NothingNotMatched { condition }
                if matches!(match_kind, MergePairKind::NotMatchedByTarget) =>
            {
                (condition.as_ref(), true)
            }
            MergeWhenPlan::UpdateNotMatchedBySource { condition, .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition }
                if matches!(match_kind, MergePairKind::NotMatchedBySource) =>
            {
                (condition.as_ref(), true)
            }
            _ => (None, false),
        };
        if !applies {
            continue;
        }
        if let Some(condition) = condition {
            let value = eval_mutation_expr(engine, ctes, condition, Some(action_row), params)?;
            if !uqa_sql::expr::truthy(&value) {
                continue;
            }
        }
        return match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                let doc_id = doc_id.ok_or_else(|| {
                    SQLError::Internal("MERGE update lost its target identity".into())
                })?;
                let old_document = target_document
                    .cloned()
                    .ok_or_else(|| missing_document_error("MERGE update", target_table, doc_id))?;
                let mut new_document = old_document.clone();
                for assignment in assignments {
                    let value = eval_mutation_assignment(
                        engine,
                        ctes,
                        MutationAssignmentTarget {
                            table: target_table,
                            column: &assignment.column,
                            action: "MERGE UPDATE",
                        },
                        &assignment.value,
                        Some(action_row),
                        params,
                    )?;
                    if let Some(value) = value {
                        new_document.insert(assignment.column.clone(), value);
                    } else {
                        new_document.remove(&assignment.column);
                    }
                }
                Ok(SelectedMergeAction::Update {
                    doc_id,
                    old_document,
                    new_document,
                    updated_columns: assignments
                        .iter()
                        .map(|assignment| assignment.column.clone())
                        .collect(),
                })
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => Ok(SelectedMergeAction::Delete {
                doc_id: doc_id.ok_or_else(|| {
                    SQLError::Internal("MERGE delete lost its target identity".into())
                })?,
            }),
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                let implicit_columns = columns.is_empty();
                let target_columns = if implicit_columns {
                    engine
                        .try_table_columns(target_table)
                        .map_err(|error| dml_storage_error("MERGE INSERT", error))?
                } else {
                    columns.clone()
                };
                if values.len() > target_columns.len()
                    || (!implicit_columns && values.len() != target_columns.len())
                {
                    return Err(SQLError::TypeMismatch(format!(
                        "MERGE INSERT row width {} != column count {}",
                        values.len(),
                        target_columns.len()
                    )));
                }
                validate_mutation_columns(
                    engine,
                    target_table,
                    target_columns.iter().map(String::as_str),
                    "MERGE INSERT",
                )?;
                let mut document = Document::new();
                for (index, column) in target_columns.iter().take(values.len()).enumerate() {
                    let value = eval_mutation_assignment(
                        engine,
                        ctes,
                        MutationAssignmentTarget {
                            table: target_table,
                            column,
                            action: "MERGE INSERT",
                        },
                        &values[index],
                        Some(action_row),
                        params,
                    )?;
                    if let Some(value) = value {
                        document.insert(column.clone(), value);
                    }
                }
                apply_missing_column_defaults(engine, target_table, &mut document, params)?;
                Ok(SelectedMergeAction::Insert { document })
            }
            MergeWhenPlan::NothingMatched { .. }
            | MergeWhenPlan::NothingNotMatched { .. }
            | MergeWhenPlan::NothingNotMatchedBySource { .. } => Ok(SelectedMergeAction::Nothing),
        };
    }
    Ok(SelectedMergeAction::Nothing)
}

fn merge_target_lock_strength(
    engine: &Engine,
    stmt: &MergePlan,
    target_table: &str,
) -> uqa_sql::ast::LockStrength {
    if stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::DeleteNotMatchedBySource { .. }
        )
    }) {
        return uqa_sql::ast::LockStrength::ForUpdate;
    }
    let columns = stmt
        .when_clauses
        .iter()
        .filter_map(|clause| match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => Some(assignments),
            _ => None,
        })
        .flatten()
        .map(|assignment| assignment.column.clone())
        .collect::<Vec<_>>();
    if columns.is_empty() {
        uqa_sql::ast::LockStrength::ForUpdate
    } else {
        update_lock_strength(engine, target_table, &columns)
    }
}
