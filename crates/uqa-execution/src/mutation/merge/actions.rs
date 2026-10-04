//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select the first matching MERGE clause and evaluate typed mutation assignments.
use super::codec::MergePairKind;
use super::model::{MergeTargetIdentity, SelectedMergeAction};
use crate::mutation::{
    assignment::{
        apply_missing_column_defaults, eval_mutation_assignment, MutationAssignmentContext,
        MutationAssignmentTarget,
    },
    errors::{dml_storage_error, missing_document_error},
    expressions::eval_mutation_expr,
};
use crate::query::CteScope;
use std::collections::BTreeSet;
use uqa_sql::{
    assignment::columns::validate_mutation_targets,
    plan::{MergePlan, MergeWhenPlan},
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;

fn merge_cardinality_violation() -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "21000".into(),
        message: "MERGE command cannot affect row a second time".into(),
        detail: None,
        hint: Some("Ensure that not more than one source row matches any one target row.".into()),
    }
}

pub(super) fn ensure_merge_target_is_modified_once(
    mutated_target_ids: &mut BTreeSet<MergeTargetIdentity>,
    storage_table: &str,
    doc_id: uqa_core::DocId,
) -> Result<(), SQLError> {
    if mutated_target_ids.insert((storage_table.to_string(), doc_id)) {
        return Ok(());
    }
    Err(merge_cardinality_violation())
}

/// Reject an action on a target row that the statement already modified, which `PostgreSQL`'s `ExecMergeMatched` finds as `TM_SelfModified` before any BEFORE ROW trigger fires for it: a row another command of the statement wrote, which a MERGE may not affect a second time, or a row that a statement the command's triggers or functions started wrote under a later command id. `event` is the action's operation, whose BEFORE ROW triggers fetch the row through `GetTupleForTrigger`.
pub(super) fn ensure_merge_target_is_unwritten<S: Clone>(
    scope: &crate::query::CteScope<S>,
    triggers: &crate::mutation::triggers::context::TriggerContext<'_>,
    storage_table: &str,
    doc_id: uqa_core::DocId,
    event: uqa_sql::ast::TriggerEvent,
) -> Result<(), SQLError> {
    let row = crate::mutation::candidate::PhysicalDocumentIdentity {
        table: storage_table.to_string(),
        doc_id,
    };
    if scope.statement_wrote(&row) {
        return Err(merge_cardinality_violation());
    }
    if scope.statement_triggered_write(&row) {
        let operation = if crate::mutation::triggers::has_before_row_triggers(
            triggers,
            storage_table,
            event,
        )? {
            "updated"
        } else {
            "updated or deleted"
        };
        return Err(crate::mutation::errors::triggered_modification_error(
            operation,
        ));
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps DML row-image inputs aligned"
)]
#[expect(clippy::too_many_lines, reason = "preserves DML lock and event order")]
pub(super) fn select_merge_action<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    stmt: &MergePlan,
    target_table: &str,
    match_kind: MergePairKind,
    doc_id: Option<uqa_core::DocId>,
    target_document: Option<&Document>,
    action_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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
            let value = eval_mutation_expr(
                services.expressions,
                ctes,
                condition,
                Some(action_row),
                params,
            )?;
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
                for (position, assignment) in assignments.iter().enumerate() {
                    let value = eval_mutation_assignment(
                        services,
                        ctes,
                        MutationAssignmentTarget {
                            table: target_table,
                            target: &assignment.target,
                            current: new_document.get(&assignment.target.column),
                            final_column_write: !assignments[position + 1..]
                                .iter()
                                .any(|next| next.target.column == assignment.target.column),
                            action: "MERGE UPDATE",
                            new_row: false,
                        },
                        &assignment.value,
                        Some(action_row),
                        params,
                    )?;
                    if let Some(value) = value {
                        new_document.insert(assignment.target.column.clone(), value);
                    } else {
                        new_document.remove(&assignment.target.column);
                    }
                }
                Ok(SelectedMergeAction::Update {
                    doc_id,
                    old_document,
                    new_document,
                    updated_columns: assignments
                        .iter()
                        .map(|assignment| assignment.target.column.clone())
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
                columns,
                overriding,
                values,
                ..
            } => {
                let implicit_columns = columns.is_empty();
                let target_columns = if implicit_columns {
                    services
                        .rows
                        .relations
                        .column_names(target_table)
                        .map_err(|error| dml_storage_error("MERGE INSERT", error))?
                        .into_iter()
                        .map(Into::into)
                        .collect()
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
                validate_mutation_targets(
                    services.columns,
                    target_table,
                    target_columns.iter(),
                    "MERGE INSERT",
                    true,
                )?;
                let mut document = Document::new();
                for (index, column) in target_columns.iter().take(values.len()).enumerate() {
                    // `OVERRIDING USER VALUE` leaves an identity column to its sequence without evaluating the value the action supplies.
                    if *overriding == Some(uqa_sql::ast::OverridingKind::UserValue)
                        && uqa_sql::assignment::columns::identity_column_sequence(
                            services.columns,
                            target_table,
                            &column.column,
                        )?
                        .is_some()
                    {
                        continue;
                    }
                    let value = eval_mutation_assignment(
                        services,
                        ctes,
                        MutationAssignmentTarget {
                            table: target_table,
                            target: column,
                            current: document.get(&column.column),
                            final_column_write: !target_columns[index + 1..]
                                .iter()
                                .any(|next| next.column == column.column),
                            action: "MERGE INSERT",
                            new_row: true,
                        },
                        &values[index],
                        Some(action_row),
                        params,
                    )?;
                    if let Some(value) = value {
                        document.insert(column.column.clone(), value);
                    }
                }
                apply_missing_column_defaults(services, target_table, &mut document, params)?;
                Ok(SelectedMergeAction::Insert {
                    document,
                    overriding: *overriding,
                })
            }
            MergeWhenPlan::NothingMatched { .. }
            | MergeWhenPlan::NothingNotMatched { .. }
            | MergeWhenPlan::NothingNotMatchedBySource { .. } => Ok(SelectedMergeAction::Nothing),
        };
    }
    Ok(SelectedMergeAction::Nothing)
}
