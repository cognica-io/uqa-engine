//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic-view rewrite-rule projection, batching, and `RETURNING` capture.

use crate::{
    mutation::{
        returning::{DmlReturningShape, ReturningExecutionContext},
        rules::{RuleContext, RuleInputProjection},
        views::ViewRowContext,
    },
    query::CteScope,
};
use std::collections::BTreeSet;
use uqa_core::{DocId, Value};
use uqa_sql::{
    plan::{ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan},
    RowSchema, SQLError, SQLParam, SQLResult,
};
use uqa_storage::document_store::Document;
#[derive(Clone)]
pub struct ViewRuleContext<'a, S: Clone + 'static> {
    pub rules: RuleContext<'a>,
    pub views: ViewRowContext<'a, S>,
}
impl<S: Clone + 'static> Copy for ViewRuleContext<'_, S> {}
struct PreparedViewRuleLayer {
    relation: String,
    batch: crate::mutation::rules::PreparedRuleBatch,
}

pub struct PreparedViewRuleBatches {
    event: uqa_sql::ast::RuleEvent,
    layers: Vec<PreparedViewRuleLayer>,
    suppress_original: Vec<bool>,
}

pub struct ViewRuleReturningCapture {
    plan: ViewRuleReturningPlan,
    result: crate::mutation::rules::RuleReturningResult,
}

pub struct ViewRuleExecutionOutcome {
    pub returning: Option<ViewRuleReturningCapture>,
    pub affected_rows: u64,
    pub sets_command_tag: bool,
}

impl ViewRuleReturningCapture {
    pub fn project<S: Clone + 'static>(
        self,
        context: ReturningExecutionContext<'_, S>,
        params: &[SQLParam],
        ctes: &CteScope<S>,
        supplemental_schema: Option<&RowSchema>,
    ) -> Result<SQLResult, SQLError> {
        let mut returning_scope = ctes.clone();
        returning_scope
            .scalar_subqueries
            .clone_from(&self.plan.subqueries);
        self.result.project(
            context,
            DmlReturningShape {
                table: &self.plan.relation,
                target_qualifier: &self.plan.target_qualifier,
                aliases: &self.plan.aliases,
                returning: &self.plan.returning,
                params,
                ctes: &returning_scope,
                supplemental_schema,
            },
        )
    }
}

impl PreparedViewRuleBatches {
    fn empty(row_count: usize, event: uqa_sql::ast::RuleEvent) -> Self {
        Self {
            event,
            layers: Vec::new(),
            suppress_original: vec![false; row_count],
        }
    }

    pub fn suppresses(&self, index: usize) -> bool {
        self.suppress_original.get(index).copied().unwrap_or(false)
    }

    pub fn configure_action_qualification(&mut self, row_independent_count: Option<usize>) {
        debug_assert!(matches!(
            self.event,
            uqa_sql::ast::RuleEvent::Update | uqa_sql::ast::RuleEvent::Delete
        ));
        for layer in &mut self.layers {
            let count = row_independent_count.unwrap_or_else(|| layer.batch.event_row_count());
            layer.batch.set_action_qualification_count(count);
        }
    }

    pub fn execute_actions_with_affected(
        &self,
        context: RuleContext<'_>,
        returning: Option<&ViewRuleReturningPlan>,
    ) -> Result<ViewRuleExecutionOutcome, SQLError> {
        let mut captured = None;
        let mut affected_rows = 0_u64;
        let mut sets_command_tag = false;
        let layers = if self.event == uqa_sql::ast::RuleEvent::Insert {
            self.layers.iter().rev().collect::<Vec<_>>()
        } else {
            self.layers.iter().collect::<Vec<_>>()
        };
        for layer in layers {
            let request = returning
                .filter(|plan| plan.relation == layer.relation)
                .map_or_else(
                    crate::mutation::rules::RuleReturningRequest::default,
                    |plan| {
                        crate::mutation::rules::RuleReturningRequest::from_plan(
                            &plan.returning,
                            &plan.aliases,
                            &plan.subqueries,
                        )
                    },
                );
            let outcome = layer
                .batch
                .execute_actions_with_affected(context, request)?;
            if outcome.sets_command_tag {
                affected_rows = outcome.affected_rows;
                sets_command_tag = true;
            }
            let Some(result) = outcome.returning else {
                continue;
            };
            if captured.is_some() {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "cannot have RETURNING lists in multiple rules".into(),
                });
            }
            let plan = returning
                .filter(|plan| plan.relation == layer.relation)
                .cloned()
                .ok_or_else(|| {
                    SQLError::Internal(
                        "automatic-view rule captured RETURNING without an outer projection".into(),
                    )
                })?;
            captured = Some(ViewRuleReturningCapture { plan, result });
        }
        Ok(ViewRuleExecutionOutcome {
            returning: captured,
            affected_rows,
            sets_command_tag,
        })
    }
}

#[derive(Clone)]
struct ViewRuleDocumentProjection<'a, S: Clone + 'static> {
    context: ViewRuleContext<'a, S>,
    relation: &'a str,
    document_relation: Option<&'a str>,
    required_columns: &'a BTreeSet<String>,
    insert_plan: Option<&'a ViewRuleInsertPlan>,
    update_plan: Option<&'a ViewRuleUpdatePlan>,
    params: &'a [SQLParam],
    scope: &'a CteScope<S>,
}

pub struct ViewRuleBatchRequest<'a, S: Clone + 'static> {
    pub context: ViewRuleContext<'a, S>,
    pub relations: &'a [String],
    pub event: uqa_sql::ast::RuleEvent,
    pub rows: &'a [crate::mutation::rules::RuleRowImage],
    pub params: &'a [SQLParam],
    pub scope: &'a CteScope<S>,
    pub insert_plans: &'a [ViewRuleInsertPlan],
    pub update_plans: &'a [ViewRuleUpdatePlan],
    pub document_relation: Option<&'a str>,
}

fn project_view_rule_expressions<S: Clone + 'static>(
    projection: &ViewRuleDocumentProjection<'_, S>,
    side: crate::mutation::rules::RuleRowSide,
    storage_table: Option<&str>,
    doc_id: Option<DocId>,
    document: Option<&Document>,
    input_document: Option<&Document>,
) -> Result<Option<RuleInputProjection>, SQLError> {
    document
        .map(|document| {
            let input_document = input_document.unwrap_or(document);
            let mut values = Vec::new();
            let mut required = projection.required_columns.clone();
            let assignments = if let Some(insert) = projection.insert_plan {
                Some((&insert.supplied_columns, &insert.input_columns, true))
            } else if matches!(side, crate::mutation::rules::RuleRowSide::New) {
                projection
                    .update_plan
                    .map(|update| (&update.assigned_columns, &update.input_columns, false))
            } else {
                None
            };
            if let Some((assigned, inputs, insert)) = assignments {
                let schema = projection
                    .context
                    .views
                    .rows
                    .relations
                    .view_schema(projection.relation)?;
                for column in projection.required_columns {
                    let value = assigned
                        .iter()
                        .position(|name| name == column)
                        .and_then(|index| inputs.get(index))
                        .and_then(|input| input_document.get(input));
                    if !insert && value.is_none() {
                        continue;
                    }
                    let value = value.cloned().unwrap_or(Value::Null);
                    let ty = schema
                        .unqualified_position(column)
                        .and_then(|position| schema.column_type(position));
                    values.push((column.clone(), value, ty.cloned()));
                    required.remove(column);
                }
            }
            let mut projected = RuleInputProjection::values(values);
            if projection.insert_plan.is_none() {
                let other = crate::mutation::views::automatic_view_rule_expressions(
                    crate::mutation::views::AutomaticViewRuleDocument {
                        context: projection.context.views,
                        view: projection.relation,
                        document_relation: projection.document_relation,
                        storage_table,
                        doc_id,
                        document,
                        required_columns: &required,
                        params: projection.params,
                        scope: projection.scope,
                    },
                )?;
                super::inputs::append_source(&mut projected.source, &other.source);
                projected.expressions.extend(other.expressions);
            }
            Ok(projected)
        })
        .transpose()
}

fn project_view_rule_document<S: Clone + 'static>(
    projection: &ViewRuleDocumentProjection<'_, S>,
    side: crate::mutation::rules::RuleRowSide,
    storage_table: Option<&str>,
    doc_id: Option<DocId>,
    document: Option<&Document>,
    input_document: Option<&Document>,
) -> Result<Option<Document>, SQLError> {
    project_view_rule_expressions(
        projection,
        side,
        storage_table,
        doc_id,
        document,
        input_document,
    )?
    .map(|projected| {
        projected
            .expressions
            .into_iter()
            .map(|(name, expression)| {
                crate::query::catalog_expression::eval_stored_expression_plan_with_row(
                    projection.context.views.expressions.expressions,
                    projection.scope.clone(),
                    &expression,
                    &projected.source.schema,
                    &projected.source.row,
                    projection.params,
                )
                .map(|value| (name, value))
            })
            .collect()
    })
    .transpose()
}

fn project_view_rule_row<S: Clone + 'static>(
    projection: &ViewRuleDocumentProjection<'_, S>,
    row: &crate::mutation::rules::RuleRowImage,
) -> Result<crate::mutation::rules::RuleRowImage, SQLError> {
    Ok(crate::mutation::rules::RuleRowImage {
        old_storage_table: row.old_storage_table.clone(),
        old_doc_id: row.old_doc_id,
        old: project_view_rule_document(
            projection,
            crate::mutation::rules::RuleRowSide::Old,
            row.old_storage_table.as_deref(),
            row.old_doc_id,
            row.old.as_ref(),
            row.old.as_ref(),
        )?,
        new_storage_table: row.new_storage_table.clone(),
        new_doc_id: row.new_doc_id,
        new: project_view_rule_document(
            projection,
            crate::mutation::rules::RuleRowSide::New,
            if projection.update_plan.is_some() {
                row.old_storage_table.as_deref()
            } else {
                row.new_storage_table.as_deref()
            },
            if projection.update_plan.is_some() {
                row.old_doc_id
            } else {
                row.new_doc_id
            },
            if projection.update_plan.is_some() {
                row.old.as_ref()
            } else {
                row.new.as_ref()
            },
            row.new.as_ref(),
        )?,
        context: row.context.clone(),
    })
}

fn project_view_rule_row_sides<S: Clone + 'static>(
    projection: &ViewRuleDocumentProjection<'_, S>,
    row: &crate::mutation::rules::RuleRowImage,
    old_columns: &BTreeSet<String>,
    new_columns: &BTreeSet<String>,
) -> Result<crate::mutation::rules::RuleRowImage, SQLError> {
    let old_projection = ViewRuleDocumentProjection {
        required_columns: old_columns,
        ..*projection
    };
    let new_projection = ViewRuleDocumentProjection {
        required_columns: new_columns,
        ..*projection
    };
    Ok(crate::mutation::rules::RuleRowImage {
        old_storage_table: row.old_storage_table.clone(),
        old_doc_id: row.old_doc_id,
        old: project_view_rule_document(
            &old_projection,
            crate::mutation::rules::RuleRowSide::Old,
            row.old_storage_table.as_deref(),
            row.old_doc_id,
            row.old.as_ref(),
            row.old.as_ref(),
        )?,
        new_storage_table: row.new_storage_table.clone(),
        new_doc_id: row.new_doc_id,
        new: project_view_rule_document(
            &new_projection,
            crate::mutation::rules::RuleRowSide::New,
            if new_projection.update_plan.is_some() {
                row.old_storage_table.as_deref()
            } else {
                row.new_storage_table.as_deref()
            },
            if new_projection.update_plan.is_some() {
                row.old_doc_id
            } else {
                row.new_doc_id
            },
            if new_projection.update_plan.is_some() {
                row.old.as_ref()
            } else {
                row.new.as_ref()
            },
            row.new.as_ref(),
        )?,
        context: row.context.clone(),
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves view qualifier and row identity"
)]
pub fn prepare_view_rule_batches<S: Clone + 'static>(
    request: ViewRuleBatchRequest<'_, S>,
) -> Result<PreparedViewRuleBatches, SQLError> {
    let ViewRuleBatchRequest {
        context,
        relations,
        event,
        rows,
        params,
        scope,
        insert_plans,
        update_plans,
        document_relation,
    } = request;
    if relations.is_empty() {
        return Ok(PreparedViewRuleBatches::empty(rows.len(), event));
    }
    let mut prepared = PreparedViewRuleBatches::empty(rows.len(), event);
    for relation in relations {
        let required_columns = BTreeSet::new();
        let insert_plan = insert_plans.iter().find(|plan| plan.relation == *relation);
        let update_plan = update_plans.iter().find(|plan| plan.relation == *relation);
        let projection = ViewRuleDocumentProjection {
            context,
            relation,
            document_relation,
            required_columns: &required_columns,
            insert_plan,
            update_plan,
            params,
            scope,
        };
        let row_indices = (0..rows.len())
            .filter(|index| !prepared.suppress_original[*index])
            .collect::<Vec<_>>();
        let view_rows = row_indices
            .iter()
            .map(|index| {
                let row = rows.get(*index).ok_or_else(|| {
                    SQLError::Internal("automatic-view rule lost its event row".into())
                })?;
                project_view_rule_row(&projection, row)
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let mut batch = crate::mutation::rules::prepare_rule_batch_with_projection(
            context.rules,
            relation,
            event,
            view_rows,
            |local_index, side, required| {
                let row_index = row_indices.get(local_index).copied().ok_or_else(|| {
                    SQLError::Internal("automatic-view rule lost its event row".into())
                })?;
                let row = rows.get(row_index).ok_or_else(|| {
                    SQLError::Internal("automatic-view rule lost its event row".into())
                })?;
                let (storage_table, doc_id, document, input_document) = match side {
                    crate::mutation::rules::RuleRowSide::Old => (
                        row.old_storage_table.as_deref(),
                        row.old_doc_id,
                        row.old.as_ref(),
                        row.old.as_ref(),
                    ),
                    crate::mutation::rules::RuleRowSide::New
                        if projection.update_plan.is_some() =>
                    {
                        (
                            row.old_storage_table.as_deref(),
                            row.old_doc_id,
                            row.old.as_ref(),
                            row.new.as_ref(),
                        )
                    }
                    crate::mutation::rules::RuleRowSide::New => (
                        row.new_storage_table.as_deref(),
                        row.new_doc_id,
                        row.new.as_ref(),
                        row.new.as_ref(),
                    ),
                };
                let Some(document) = document else {
                    return Ok(RuleInputProjection::default());
                };
                let projection = ViewRuleDocumentProjection {
                    required_columns: required,
                    ..projection
                };
                let projected = project_view_rule_expressions(
                    &projection,
                    side,
                    storage_table,
                    doc_id,
                    Some(document),
                    input_document,
                )?
                .ok_or_else(|| {
                    SQLError::Internal("automatic-view rule projection lost its row".into())
                })?;
                Ok(projected)
            },
        )?;
        let action_columns = batch.missing_action_row_columns();
        if action_columns
            .iter()
            .any(|(old, new)| !old.is_empty() || !new.is_empty())
        {
            let supplemental_rows = row_indices
                .iter()
                .zip(action_columns)
                .map(|(index, (old_columns, new_columns))| {
                    let row = rows.get(*index).ok_or_else(|| {
                        SQLError::Internal("automatic-view rule lost its event row".into())
                    })?;
                    project_view_rule_row_sides(&projection, row, &old_columns, &new_columns)
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            batch.supplement_rows(supplemental_rows)?;
        }
        for (local_index, row_index) in row_indices.iter().copied().enumerate() {
            if batch.suppresses(local_index) {
                prepared.suppress_original[row_index] = true;
            }
        }
        prepared.layers.push(PreparedViewRuleLayer {
            relation: relation.clone(),
            batch,
        });
        if uqa_sql::semantics::rules::analysis::relation_suppresses_original_query(
            context.rules.analysis,
            relation,
            event,
        )? {
            break;
        }
    }
    Ok(prepared)
}

impl<S: Clone + 'static> Copy for ViewRuleDocumentProjection<'_, S> {}
