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
    assignment::columns::validate_mutation_columns,
    plan::{MergePlan, MergeWhenPlan},
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;

pub(super) fn ensure_merge_target_is_modified_once(
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
                for assignment in assignments {
                    let value = eval_mutation_assignment(
                        services,
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
                    services
                        .rows
                        .relations
                        .column_names(target_table)
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
                    services.columns,
                    target_table,
                    target_columns.iter().map(String::as_str),
                    "MERGE INSERT",
                )?;
                let mut document = Document::new();
                for (index, column) in target_columns.iter().take(values.len()).enumerate() {
                    let value = eval_mutation_assignment(
                        services,
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
                apply_missing_column_defaults(services, target_table, &mut document, params)?;
                Ok(SelectedMergeAction::Insert { document })
            }
            MergeWhenPlan::NothingMatched { .. }
            | MergeWhenPlan::NothingNotMatched { .. }
            | MergeWhenPlan::NothingNotMatchedBySource { .. } => Ok(SelectedMergeAction::Nothing),
        };
    }
    Ok(SelectedMergeAction::Nothing)
}
