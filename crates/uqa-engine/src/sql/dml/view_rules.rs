//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic-view rewrite-rule projection, batching, and `RETURNING` capture.

use super::{
    view_automatic, BTreeSet, CteScope, DmlReturningShape, DocId, Document, Engine, RowSchema,
    SQLError, SQLParam, SQLResult, Value, ViewRuleInsertPlan, ViewRuleReturningPlan,
    ViewRuleUpdatePlan,
};

struct PreparedViewRuleLayer {
    relation: String,
    batch: crate::sql::rules::PreparedRuleBatch,
}

pub(super) struct PreparedViewRuleBatches {
    event: uqa_sql::ast::RuleEvent,
    layers: Vec<PreparedViewRuleLayer>,
    suppress_original: Vec<bool>,
}

pub(super) struct ViewRuleReturningCapture {
    plan: ViewRuleReturningPlan,
    result: crate::sql::rules::RuleReturningResult,
}

pub(super) struct ViewRuleExecutionOutcome {
    pub(super) returning: Option<ViewRuleReturningCapture>,
    pub(super) affected_rows: u64,
    pub(super) executed_action: bool,
}

impl ViewRuleReturningCapture {
    pub(super) fn project(
        self,
        engine: &Engine,
        params: &[SQLParam],
        ctes: &CteScope,
        supplemental_schema: Option<&RowSchema>,
    ) -> Result<SQLResult, SQLError> {
        let mut returning_scope = ctes.clone();
        returning_scope
            .scalar_subqueries
            .clone_from(&self.plan.subqueries);
        self.result.project(
            engine,
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

    pub(super) fn suppresses(&self, index: usize) -> bool {
        self.suppress_original.get(index).copied().unwrap_or(false)
    }

    pub(super) fn configure_action_qualification(&mut self, row_independent_count: Option<usize>) {
        debug_assert!(matches!(
            self.event,
            uqa_sql::ast::RuleEvent::Update | uqa_sql::ast::RuleEvent::Delete
        ));
        for layer in &mut self.layers {
            let count = row_independent_count.unwrap_or_else(|| layer.batch.event_row_count());
            layer.batch.set_action_qualification_count(count);
        }
    }

    pub(super) fn execute_actions(
        &self,
        engine: &Engine,
        returning: Option<&ViewRuleReturningPlan>,
    ) -> Result<Option<ViewRuleReturningCapture>, SQLError> {
        Ok(self
            .execute_actions_with_affected(engine, returning)?
            .returning)
    }

    pub(super) fn execute_actions_with_affected(
        &self,
        engine: &Engine,
        returning: Option<&ViewRuleReturningPlan>,
    ) -> Result<ViewRuleExecutionOutcome, SQLError> {
        let mut captured = None;
        let mut affected_rows = 0_u64;
        let mut executed_action = false;
        let layers = if self.event == uqa_sql::ast::RuleEvent::Insert {
            self.layers.iter().rev().collect::<Vec<_>>()
        } else {
            self.layers.iter().collect::<Vec<_>>()
        };
        for layer in layers {
            let request = returning
                .filter(|plan| plan.relation == layer.relation)
                .map_or_else(crate::sql::rules::RuleReturningRequest::default, |plan| {
                    crate::sql::rules::RuleReturningRequest::from_plan(
                        &plan.returning,
                        &plan.aliases,
                        &plan.subqueries,
                    )
                });
            let outcome = layer.batch.execute_actions_with_affected(engine, request)?;
            if outcome.executed_action {
                affected_rows = outcome.affected_rows;
                executed_action = true;
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
            executed_action,
        })
    }
}

#[derive(Clone, Copy)]
struct ViewRuleDocumentProjection<'a> {
    engine: &'a Engine,
    relation: &'a str,
    document_relation: Option<&'a str>,
    required_columns: &'a BTreeSet<String>,
    insert_plan: Option<&'a ViewRuleInsertPlan>,
    update_plan: Option<&'a ViewRuleUpdatePlan>,
    params: &'a [SQLParam],
    scope: &'a CteScope,
}

pub(super) struct ViewRuleBatchRequest<'a> {
    pub(super) engine: &'a Engine,
    pub(super) relations: &'a [String],
    pub(super) event: uqa_sql::ast::RuleEvent,
    pub(super) rows: &'a [crate::sql::rules::RuleRowImage],
    pub(super) params: &'a [SQLParam],
    pub(super) scope: &'a CteScope,
    pub(super) insert_plans: &'a [ViewRuleInsertPlan],
    pub(super) update_plans: &'a [ViewRuleUpdatePlan],
    pub(super) document_relation: Option<&'a str>,
}

fn project_view_rule_document(
    projection: &ViewRuleDocumentProjection<'_>,
    side: crate::sql::rules::RuleRowSide,
    storage_table: Option<&str>,
    doc_id: Option<DocId>,
    document: Option<&Document>,
    input_document: Option<&Document>,
) -> Result<Option<Document>, SQLError> {
    document
        .map(|document| {
            if let Some(insert_plan) = projection.insert_plan {
                let input_document = input_document.unwrap_or(document);
                let mut projected = Document::new();
                for column in projection.required_columns {
                    let value = insert_plan
                        .supplied_columns
                        .iter()
                        .position(|supplied| supplied == column)
                        .and_then(|position| insert_plan.input_columns.get(position))
                        .and_then(|input| input_document.get(input))
                        .cloned()
                        .unwrap_or(Value::Null);
                    projected.insert(column.clone(), value);
                }
                return Ok(projected);
            }
            let mut projected = view_automatic::automatic_view_rule_document(
                view_automatic::AutomaticViewRuleDocument {
                    engine: projection.engine,
                    view: projection.relation,
                    document_relation: projection.document_relation,
                    storage_table,
                    doc_id,
                    document,
                    required_columns: projection.required_columns,
                    params: projection.params,
                    scope: projection.scope,
                },
            )?;
            if matches!(side, crate::sql::rules::RuleRowSide::New) {
                if let Some(update_plan) = projection.update_plan {
                    let input_document = input_document.unwrap_or(document);
                    for column in projection.required_columns {
                        let Some(position) = update_plan
                            .assigned_columns
                            .iter()
                            .position(|assigned| assigned == column)
                        else {
                            continue;
                        };
                        let Some(input) = update_plan.input_columns.get(position) else {
                            continue;
                        };
                        if let Some(value) = input_document.get(input) {
                            projected.insert(column.clone(), value.clone());
                        }
                    }
                }
            }
            Ok(projected)
        })
        .transpose()
}

fn project_view_rule_row(
    projection: &ViewRuleDocumentProjection<'_>,
    row: &crate::sql::rules::RuleRowImage,
) -> Result<crate::sql::rules::RuleRowImage, SQLError> {
    Ok(crate::sql::rules::RuleRowImage {
        old_storage_table: row.old_storage_table.clone(),
        old_doc_id: row.old_doc_id,
        old: project_view_rule_document(
            projection,
            crate::sql::rules::RuleRowSide::Old,
            row.old_storage_table.as_deref(),
            row.old_doc_id,
            row.old.as_ref(),
            row.old.as_ref(),
        )?,
        new_storage_table: row.new_storage_table.clone(),
        new_doc_id: row.new_doc_id,
        new: project_view_rule_document(
            projection,
            crate::sql::rules::RuleRowSide::New,
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

fn project_view_rule_row_sides(
    projection: &ViewRuleDocumentProjection<'_>,
    row: &crate::sql::rules::RuleRowImage,
    old_columns: &BTreeSet<String>,
    new_columns: &BTreeSet<String>,
) -> Result<crate::sql::rules::RuleRowImage, SQLError> {
    let old_projection = ViewRuleDocumentProjection {
        required_columns: old_columns,
        ..*projection
    };
    let new_projection = ViewRuleDocumentProjection {
        required_columns: new_columns,
        ..*projection
    };
    Ok(crate::sql::rules::RuleRowImage {
        old_storage_table: row.old_storage_table.clone(),
        old_doc_id: row.old_doc_id,
        old: project_view_rule_document(
            &old_projection,
            crate::sql::rules::RuleRowSide::Old,
            row.old_storage_table.as_deref(),
            row.old_doc_id,
            row.old.as_ref(),
            row.old.as_ref(),
        )?,
        new_storage_table: row.new_storage_table.clone(),
        new_doc_id: row.new_doc_id,
        new: project_view_rule_document(
            &new_projection,
            crate::sql::rules::RuleRowSide::New,
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
pub(super) fn prepare_view_rule_batches(
    request: ViewRuleBatchRequest<'_>,
) -> Result<PreparedViewRuleBatches, SQLError> {
    let ViewRuleBatchRequest {
        engine,
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
            engine,
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
        let mut batch = crate::sql::rules::prepare_rule_batch_with_projection(
            engine,
            relation,
            event,
            view_rows,
            |local_index, side, column| {
                let row_index = row_indices.get(local_index).copied().ok_or_else(|| {
                    SQLError::Internal("automatic-view rule lost its event row".into())
                })?;
                let row = rows.get(row_index).ok_or_else(|| {
                    SQLError::Internal("automatic-view rule lost its event row".into())
                })?;
                let (storage_table, doc_id, document, input_document) = match side {
                    crate::sql::rules::RuleRowSide::Old => (
                        row.old_storage_table.as_deref(),
                        row.old_doc_id,
                        row.old.as_ref(),
                        row.old.as_ref(),
                    ),
                    crate::sql::rules::RuleRowSide::New if projection.update_plan.is_some() => (
                        row.old_storage_table.as_deref(),
                        row.old_doc_id,
                        row.old.as_ref(),
                        row.new.as_ref(),
                    ),
                    crate::sql::rules::RuleRowSide::New => (
                        row.new_storage_table.as_deref(),
                        row.new_doc_id,
                        row.new.as_ref(),
                        row.new.as_ref(),
                    ),
                };
                let Some(document) = document else {
                    return Ok(None);
                };
                let required = BTreeSet::from([column.to_string()]);
                let projection = ViewRuleDocumentProjection {
                    required_columns: &required,
                    ..projection
                };
                let projected = project_view_rule_document(
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
                projected.get(column).cloned().map(Some).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "automatic-view rule projection omitted column `{column}`"
                    ))
                })
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
        if crate::sql::rules::relation_suppresses_original_query(engine, relation, event)? {
            break;
        }
    }
    Ok(prepared)
}
