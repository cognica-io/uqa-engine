//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::scalar::{eval_lowered_expression, eval_physical_scalar, PhysicalEvalContext};
use super::ScopedEngineHook;
use super::{
    bind_projection_output_schema, build_join_spill_with_ctes,
    build_projection_physical_row_with_ctes, coerce_to_column_type, column_type_name, doc_id_value,
    partition_insert_target, validate_vector_dimensions, value_to_tensor, value_to_vector,
    BTreeMap, BTreeSet, BinaryOp, ColumnType, CteScope, DocId, Document, Engine, ForeignKey,
    ForeignKeyAction, ForeignKeyMatch, RowIndependentUpdateValues, SQLError, SQLParam, SQLResult,
    Value, DOC_ID_COLUMN, TABLE_OID_COLUMN, XMIN_COLUMN, XMIN_STORAGE_COLUMN,
    XMIN_USER_STORAGE_COLUMN,
};
use crate::RelationIdentity;
use uqa_execution::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr};
use uqa_planner::{
    ComputePlan, ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan,
    MergeWhenPlan, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan, UpdatePlan,
    ViewCheckPlan, ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan,
};

fn prune_unused_query_outputs(
    query: &mut QueryPlan,
    required_positions: &BTreeSet<usize>,
    expected_width: usize,
) {
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        return;
    };
    let projection_can_be_pruned = matches!(block.compute, ComputePlan::Project)
        && !block.distinct
        && block.distinct_on.is_empty()
        && block.order_by.is_empty()
        && block.projections.len() == expected_width;
    if !projection_can_be_pruned {
        return;
    }
    for (position, projection) in block.projections.iter_mut().enumerate() {
        if !required_positions.contains(&position) {
            projection.expr = ScalarExpr::Literal(Value::Null);
        }
    }
}

struct PreparedViewRuleLayer {
    relation: String,
    batch: crate::sql::rules::PreparedRuleBatch,
}

struct PreparedViewRuleBatches {
    event: uqa_sql::ast::RuleEvent,
    layers: Vec<PreparedViewRuleLayer>,
    suppress_original: Vec<bool>,
}

struct ViewRuleReturningCapture {
    plan: ViewRuleReturningPlan,
    result: crate::sql::rules::RuleReturningResult,
}

struct ViewRuleExecutionOutcome {
    returning: Option<ViewRuleReturningCapture>,
    affected_rows: u64,
    executed_action: bool,
}

impl ViewRuleReturningCapture {
    fn project(
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

    fn suppresses(&self, index: usize) -> bool {
        self.suppress_original.get(index).copied().unwrap_or(false)
    }

    fn configure_action_qualification(&mut self, row_independent_count: Option<usize>) {
        debug_assert!(matches!(
            self.event,
            uqa_sql::ast::RuleEvent::Update | uqa_sql::ast::RuleEvent::Delete
        ));
        for layer in &mut self.layers {
            let count = row_independent_count.unwrap_or_else(|| layer.batch.event_row_count());
            layer.batch.set_action_qualification_count(count);
        }
    }

    fn execute_actions(
        &self,
        engine: &Engine,
        returning: Option<&ViewRuleReturningPlan>,
    ) -> Result<Option<ViewRuleReturningCapture>, SQLError> {
        Ok(self
            .execute_actions_with_affected(engine, returning)?
            .returning)
    }

    fn execute_actions_with_affected(
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

struct ViewRuleBatchRequest<'a> {
    engine: &'a Engine,
    relations: &'a [String],
    event: uqa_sql::ast::RuleEvent,
    rows: &'a [crate::sql::rules::RuleRowImage],
    params: &'a [SQLParam],
    scope: &'a CteScope,
    insert_plans: &'a [ViewRuleInsertPlan],
    update_plans: &'a [ViewRuleUpdatePlan],
    document_relation: Option<&'a str>,
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

fn prepare_view_rule_batches(
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

fn dml_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

pub(in crate::sql) fn concurrent_update_serialization_failure() -> SQLError {
    SQLError::Routine {
        sqlstate: "40001".into(),
        message: "could not serialize access due to concurrent update".into(),
    }
}

pub(in crate::sql) struct DmlCommandMutationOverlay<'a> {
    engine: &'a Engine,
}

impl<'a> DmlCommandMutationOverlay<'a> {
    pub(in crate::sql) fn new(engine: &'a Engine) -> Self {
        engine.begin_command_mutation_overlay();
        Self { engine }
    }
}

impl Drop for DmlCommandMutationOverlay<'_> {
    fn drop(&mut self) {
        self.engine.end_command_mutation_overlay();
    }
}

fn lock_mutation_row(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<bool, SQLError> {
    match engine.lock_row(
        table,
        doc_id,
        strength,
        uqa_sql::ast::LockWait::Block,
        display_name,
    )? {
        crate::row_locks::LockAcquire::Granted { waited, .. } => Ok(waited),
        crate::row_locks::LockAcquire::Skipped => Err(SQLError::Internal(
            "DML row locking used SKIP LOCKED".into(),
        )),
    }
}

/// Outcome of following a DML target through its committed update chain.
pub(in crate::sql) enum MutationLockTarget {
    Present { doc_id: DocId, recheck: bool },
    Deleted,
}

pub(in crate::sql) enum PhysicalMutationLockTarget {
    Present {
        identity: PhysicalDocumentIdentity,
        recheck: bool,
    },
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::sql) struct PhysicalDocumentIdentity {
    pub table: String,
    pub doc_id: DocId,
}

pub(in crate::sql) struct PreparedDocumentRewrite {
    pub table: String,
    pub doc_id: DocId,
    pub destination: Option<(String, DocId)>,
    pub partition_move_delete: Option<Box<PreparedDocumentDelete>>,
    pub old_document: Document,
    pub new_document: Document,
    pub actions: Vec<PreparedDocumentRewrite>,
    pub trigger_updated_columns: Option<Vec<String>>,
    pub capture_partition_move_update_transition: bool,
}

impl PreparedDocumentRewrite {
    pub(in crate::sql) fn is_partition_move_delete(&self) -> bool {
        self.partition_move_delete.is_some()
    }
}

pub(in crate::sql) enum PreparedDeleteAction {
    Delete(Box<PreparedDocumentDelete>),
    Rewrite(Box<PreparedDocumentRewrite>),
}

pub(in crate::sql) struct PreparedDocumentDelete {
    pub table: String,
    pub doc_id: DocId,
    pub document: Document,
    pub actions: Vec<PreparedDeleteAction>,
}

#[derive(Default)]
pub(in crate::sql) struct ReferentialActionContext {
    pub(in crate::sql) delete_stack: Vec<(String, DocId)>,
    pub(in crate::sql) rewrite_stack: Vec<(String, DocId)>,
    pub(in crate::sql) trigger_statements: crate::sql::triggers::ReferentialTriggerStatements,
    pending_documents: BTreeMap<PhysicalDocumentIdentity, Option<Document>>,
}

impl ReferentialActionContext {
    pub(in crate::sql) fn pending_document(
        &self,
        identity: &PhysicalDocumentIdentity,
    ) -> Option<&Option<Document>> {
        self.pending_documents.get(identity)
    }

    pub(in crate::sql) fn record_pending_document(
        &mut self,
        identity: PhysicalDocumentIdentity,
        document: Option<Document>,
    ) {
        self.pending_documents.insert(identity, document);
    }

    pub(in crate::sql) fn transition_tables(
        &self,
        engine: &Engine,
        events: &[crate::sql::triggers::AfterRowTriggerEvent],
    ) -> Result<Vec<crate::sql::triggers::TransitionTables>, SQLError> {
        self.trigger_statements
            .build_transition_tables(engine, events)
    }

    pub(in crate::sql) fn fire_after_statement_triggers(
        &self,
        engine: &Engine,
        transitions: &[crate::sql::triggers::TransitionTables],
        root_table: &str,
        root_events: &[uqa_sql::ast::TriggerEvent],
        generation: usize,
    ) -> Result<(), SQLError> {
        self.trigger_statements
            .fire_after(engine, transitions, root_table, root_events, generation)
    }
}

pub(in crate::sql) struct ReferentialRewritePreparation<'a> {
    pub(in crate::sql) table: &'a str,
    pub(in crate::sql) doc_id: DocId,
    pub(in crate::sql) old_document: Document,
    pub(in crate::sql) proposed_document: Document,
    pub(in crate::sql) updated_columns: Vec<String>,
}

pub(in crate::sql) fn encode_prepared_doc_id(doc_id: DocId) -> Value {
    Value::Bytes(doc_id.to_be_bytes().to_vec())
}

pub(in crate::sql) fn decode_prepared_doc_id(
    value: Value,
    context: &str,
) -> Result<DocId, SQLError> {
    let Value::Bytes(bytes) = value else {
        return Err(SQLError::Internal(format!(
            "{context} has a non-binary document id"
        )));
    };
    let bytes: [u8; std::mem::size_of::<DocId>()] = bytes
        .try_into()
        .map_err(|_| SQLError::Internal(format!("{context} has an invalid document id width")))?;
    Ok(DocId::from_be_bytes(bytes))
}

pub(in crate::sql) fn encode_prepared_document_rewrite(prepared: PreparedDocumentRewrite) -> Value {
    Value::Map(BTreeMap::from([
        ("table".into(), Value::Str(prepared.table)),
        ("doc_id".into(), encode_prepared_doc_id(prepared.doc_id)),
        (
            "destination".into(),
            prepared.destination.map_or(Value::Null, |(table, doc_id)| {
                Value::Map(BTreeMap::from([
                    ("table".into(), Value::Str(table)),
                    ("doc_id".into(), encode_prepared_doc_id(doc_id)),
                ]))
            }),
        ),
        (
            "partition_move_delete".into(),
            prepared
                .partition_move_delete
                .map_or(Value::Null, |delete| {
                    encode_prepared_document_delete(*delete)
                }),
        ),
        ("old".into(), Value::Map(prepared.old_document)),
        ("new".into(), Value::Map(prepared.new_document)),
        (
            "actions".into(),
            Value::List(
                prepared
                    .actions
                    .into_iter()
                    .map(encode_prepared_document_rewrite)
                    .collect(),
            ),
        ),
        (
            "capture_partition_move_update_transition".into(),
            Value::Bool(prepared.capture_partition_move_update_transition),
        ),
    ]))
}

pub(in crate::sql) fn decode_prepared_document_rewrite(
    value: Value,
) -> Result<PreparedDocumentRewrite, SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "prepared rewrite spill payload is not a map".into(),
        ));
    };
    let table = match fields.remove("table") {
        Some(Value::Str(table)) => table,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no table".into(),
            ))
        }
    };
    let doc_id = decode_prepared_doc_id(
        fields.remove("doc_id").ok_or_else(|| {
            SQLError::Internal("prepared rewrite spill payload has no document id".into())
        })?,
        "prepared rewrite spill payload",
    )?;
    let destination = match fields.remove("destination") {
        Some(Value::Null) | None => None,
        Some(Value::Map(mut destination)) => {
            let table = match destination.remove("table") {
                Some(Value::Str(table)) => table,
                _ => {
                    return Err(SQLError::Internal(
                        "prepared rewrite destination has no table".into(),
                    ))
                }
            };
            let doc_id = decode_prepared_doc_id(
                destination.remove("doc_id").ok_or_else(|| {
                    SQLError::Internal("prepared rewrite destination has no document id".into())
                })?,
                "prepared rewrite destination",
            )?;
            Some((table, doc_id))
        }
        Some(_) => {
            return Err(SQLError::Internal(
                "prepared rewrite destination is not a map".into(),
            ))
        }
    };
    let partition_move_delete = match fields.remove("partition_move_delete") {
        Some(Value::Null) | None => None,
        Some(delete) => Some(Box::new(decode_prepared_document_delete(delete)?)),
    };
    let old_document = match fields.remove("old") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no old document".into(),
            ))
        }
    };
    let new_document = match fields.remove("new") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no new document".into(),
            ))
        }
    };
    let actions = match fields.remove("actions") {
        Some(Value::List(actions)) => actions
            .into_iter()
            .map(decode_prepared_document_rewrite)
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no action list".into(),
            ))
        }
    };
    let capture_partition_move_update_transition = match fields
        .remove("capture_partition_move_update_transition")
    {
        Some(Value::Bool(capture)) => capture,
        _ => {
            return Err(SQLError::Internal(
                "prepared rewrite spill payload has no partition movement transition mode".into(),
            ))
        }
    };
    Ok(PreparedDocumentRewrite {
        table,
        doc_id,
        destination,
        partition_move_delete,
        old_document,
        new_document,
        actions,
        trigger_updated_columns: None,
        capture_partition_move_update_transition,
    })
}

pub(in crate::sql) fn encode_prepared_document_delete(prepared: PreparedDocumentDelete) -> Value {
    let actions = prepared
        .actions
        .into_iter()
        .map(|action| match action {
            PreparedDeleteAction::Delete(delete) => Value::Map(BTreeMap::from([
                ("kind".into(), Value::Str("delete".into())),
                ("plan".into(), encode_prepared_document_delete(*delete)),
            ])),
            PreparedDeleteAction::Rewrite(rewrite) => Value::Map(BTreeMap::from([
                ("kind".into(), Value::Str("rewrite".into())),
                ("plan".into(), encode_prepared_document_rewrite(*rewrite)),
            ])),
        })
        .collect();
    Value::Map(BTreeMap::from([
        ("table".into(), Value::Str(prepared.table)),
        ("doc_id".into(), encode_prepared_doc_id(prepared.doc_id)),
        ("document".into(), Value::Map(prepared.document)),
        ("actions".into(), Value::List(actions)),
    ]))
}

pub(in crate::sql) fn decode_prepared_document_delete(
    value: Value,
) -> Result<PreparedDocumentDelete, SQLError> {
    let Value::Map(mut fields) = value else {
        return Err(SQLError::Internal(
            "prepared delete spill payload is not a map".into(),
        ));
    };
    let table = match fields.remove("table") {
        Some(Value::Str(table)) => table,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no table".into(),
            ))
        }
    };
    let doc_id = decode_prepared_doc_id(
        fields.remove("doc_id").ok_or_else(|| {
            SQLError::Internal("prepared delete spill payload has no document id".into())
        })?,
        "prepared delete spill payload",
    )?;
    let document = match fields.remove("document") {
        Some(Value::Map(document)) => document,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no document".into(),
            ))
        }
    };
    let action_values = match fields.remove("actions") {
        Some(Value::List(actions)) => actions,
        _ => {
            return Err(SQLError::Internal(
                "prepared delete spill payload has no action list".into(),
            ))
        }
    };
    let mut actions = Vec::with_capacity(action_values.len());
    for action in action_values {
        let Value::Map(mut action) = action else {
            return Err(SQLError::Internal(
                "prepared delete action spill payload is not a map".into(),
            ));
        };
        let kind = match action.remove("kind") {
            Some(Value::Str(kind)) => kind,
            _ => {
                return Err(SQLError::Internal(
                    "prepared delete action spill payload has no kind".into(),
                ))
            }
        };
        let plan = action.remove("plan").ok_or_else(|| {
            SQLError::Internal("prepared delete action spill payload has no plan".into())
        })?;
        actions.push(match kind.as_str() {
            "delete" => {
                PreparedDeleteAction::Delete(Box::new(decode_prepared_document_delete(plan)?))
            }
            "rewrite" => {
                PreparedDeleteAction::Rewrite(Box::new(decode_prepared_document_rewrite(plan)?))
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "prepared delete action spill payload has unknown kind `{kind}`"
                )))
            }
        });
    }
    Ok(PreparedDocumentDelete {
        table,
        doc_id,
        document,
        actions,
    })
}

/// Lock a DML target row and follow any primary-key rewrite another transaction committed while this statement waited, exactly like `PostgreSQL` 18 follows the update chain to the row version it lands on. Returns the doc id the statement must act on together with whether any wait or successor hop makes a re-qualification necessary. Callers acquire every row dependency first and promote the backend writer only after that lock phase has completed.
pub(in crate::sql) fn lock_mutation_target(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<MutationLockTarget, SQLError> {
    let mut current = doc_id;
    let mut recheck = false;
    let mut hops = 0usize;
    loop {
        recheck |= lock_mutation_row(engine, table, display_name, current, strength)?;
        let successor = match engine.committed_row_successor(table, current)? {
            crate::row_locks::RowChangeTarget::Unchanged => {
                return Ok(MutationLockTarget::Present {
                    doc_id: current,
                    recheck,
                });
            }
            crate::row_locks::RowChangeTarget::Deleted
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::RowChangeTarget::Deleted => {
                return Ok(MutationLockTarget::Deleted);
            }
            crate::row_locks::RowChangeTarget::Present(_)
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::RowChangeTarget::Present(successor) => successor,
        };
        if successor == current {
            return Ok(MutationLockTarget::Present {
                doc_id: current,
                recheck: true,
            });
        }
        hops += 1;
        if hops > 64 {
            return Err(SQLError::Internal(format!(
                "primary-key rewrite chain for `{table}` row {doc_id} did not converge"
            )));
        }
        recheck = true;
        current = successor;
    }
}

/// Lock a DML candidate and follow a committed update chain across physical relations. Declarative partition movement changes the leaf table as well as the document id, so callers that scan a hierarchy must retain the complete successor identity.
pub(in crate::sql) fn lock_physical_mutation_target(
    engine: &Engine,
    table: &str,
    display_name: &str,
    doc_id: DocId,
    strength: uqa_sql::ast::LockStrength,
) -> Result<PhysicalMutationLockTarget, SQLError> {
    let mut current = PhysicalDocumentIdentity {
        table: table.to_string(),
        doc_id,
    };
    let mut recheck = false;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return Err(SQLError::Internal(format!(
                "physical rewrite chain for `{display_name}` row {table}:{doc_id} contains a cycle at {}:{}",
                current.table, current.doc_id
            )));
        }
        recheck |= lock_mutation_row(
            engine,
            &current.table,
            display_name,
            current.doc_id,
            strength,
        )?;
        match engine.committed_physical_row_successor(&current.table, current.doc_id)? {
            crate::row_locks::PhysicalRowChangeTarget::Unchanged => {
                return Ok(PhysicalMutationLockTarget::Present {
                    identity: current,
                    recheck,
                });
            }
            crate::row_locks::PhysicalRowChangeTarget::Deleted
            | crate::row_locks::PhysicalRowChangeTarget::Present { .. }
                if engine.current_transaction_uses_fixed_snapshot() =>
            {
                return Err(concurrent_update_serialization_failure());
            }
            crate::row_locks::PhysicalRowChangeTarget::Deleted => {
                return Ok(PhysicalMutationLockTarget::Deleted);
            }
            crate::row_locks::PhysicalRowChangeTarget::Present { table_hash, doc_id } => {
                let table = engine.row_lock_table_for_hash(table_hash)?;
                if table == current.table && doc_id == current.doc_id {
                    return Ok(PhysicalMutationLockTarget::Present {
                        identity: current,
                        recheck: true,
                    });
                }
                recheck = true;
                current = PhysicalDocumentIdentity { table, doc_id };
            }
        }
    }
}

pub(crate) fn update_lock_strength(
    engine: &Engine,
    table: &str,
    columns: &[String],
) -> uqa_sql::ast::LockStrength {
    let Ok(keys) = engine.try_key_constraints(table) else {
        return uqa_sql::ast::LockStrength::ForUpdate;
    };
    let Ok(Some(definitions)) = engine.try_describe_table(table) else {
        return uqa_sql::ast::LockStrength::ForUpdate;
    };
    let assigned = columns.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let touches_key = keys.iter().any(|constraint| {
        constraint.columns.iter().any(|column| {
            if assigned.contains(column.as_str()) {
                return true;
            }
            let Some(generated) = definitions
                .iter()
                .find(|definition| definition.name == *column)
                .and_then(|definition| definition.generated.as_ref())
            else {
                return false;
            };
            let mut dependencies = BTreeSet::new();
            let expression = uqa_planner::ExpressionPlan::lower((*generated.expression).clone());
            !expression.scalar.collect_columns(&mut dependencies)
                || dependencies
                    .iter()
                    .any(|dependency| assigned.contains(dependency.as_str()))
        })
    });
    if touches_key {
        uqa_sql::ast::LockStrength::ForUpdate
    } else {
        uqa_sql::ast::LockStrength::ForNoKeyUpdate
    }
}

fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}

fn is_virtual_document_id_column(column: &str, definitions: &[uqa_sql::ast::ColumnDef]) -> bool {
    column == DOC_ID_COLUMN
        && !definitions
            .iter()
            .any(|definition| definition.name == DOC_ID_COLUMN)
}

pub(crate) fn stamp_tuple_xmin(
    engine: &Engine,
    table: &str,
    document: &mut Document,
) -> Result<(), SQLError> {
    let xmin = Value::Int(i64::from(engine.tuple_version_xid()?));
    let previous_system_xmin = document.get(XMIN_STORAGE_COLUMN).cloned();
    let schemaless_user_xmin_marked = document
        .get(XMIN_USER_STORAGE_COLUMN)
        .is_some_and(|value| value == &Value::Bool(true));
    let public_xmin_was_system_mirror = previous_system_xmin
        .as_ref()
        .is_some_and(|previous| document.get(XMIN_COLUMN) == Some(previous));
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("resolve tuple-version schema", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let has_declared_xmin = definitions
        .iter()
        .any(|definition| definition.name == XMIN_COLUMN);
    let schemaless_user_xmin = definitions.is_empty()
        && (schemaless_user_xmin_marked
            || (document.contains_key(XMIN_COLUMN)
                && (previous_system_xmin.is_none() || !public_xmin_was_system_mirror)));
    document.insert(XMIN_STORAGE_COLUMN.into(), xmin.clone());
    if schemaless_user_xmin {
        document.insert(XMIN_USER_STORAGE_COLUMN.into(), Value::Bool(true));
    } else {
        document.remove(XMIN_USER_STORAGE_COLUMN);
    }
    if !has_declared_xmin && !schemaless_user_xmin {
        // Keep the legacy projection mirror while old database rows are migrated lazily. The collision-free key above is authoritative and the mirror is never written when `xmin` belongs to the user schema.
        document.insert(XMIN_COLUMN.into(), xmin);
    }
    Ok(())
}

fn dml_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    dml_target_row_for_storage(engine, table, table, qualifier, doc_id, document)
}

fn dml_target_row_for_storage(
    engine: &Engine,
    table: &str,
    storage_table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    dml_target_row_for_storage_optional(
        engine,
        table,
        Some(storage_table),
        qualifier,
        Some(doc_id),
        document,
        None,
    )
}

fn dml_target_row_for_storage_optional(
    engine: &Engine,
    table: &str,
    storage_table: Option<&str>,
    qualifier: &str,
    doc_id: Option<DocId>,
    document: &Document,
    selected_columns: Option<&BTreeSet<String>>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut materialized = document.clone();
    if let Some(selected_columns) = selected_columns {
        crate::engine_generated::materialize_selected_virtual_generated_columns(
            &definitions,
            &mut materialized,
            selected_columns,
        )?;
    } else {
        crate::engine_generated::materialize_virtual_generated_columns(
            &definitions,
            &mut materialized,
        )?;
    }
    let mut columns = if definitions.is_empty() {
        materialized
            .keys()
            .filter(|column| {
                column.as_str() != XMIN_STORAGE_COLUMN
                    && column.as_str() != XMIN_USER_STORAGE_COLUMN
                    && column.as_str() != XMIN_COLUMN
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(ColumnType::Oid));
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    let values = columns
        .iter()
        .map(|column| {
            if is_virtual_document_id_column(column, &definitions)
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id.map_or(Ok(Value::Null), doc_id_value)
            } else if column == TABLE_OID_COLUMN {
                storage_table.map_or(Ok(Value::Null), |storage_table| {
                    Ok(Value::Int(crate::sql::catalog::table_relation_oid(
                        engine,
                        storage_table,
                    )?))
                })
            } else if column == XMIN_COLUMN {
                Ok(materialized
                    .get(XMIN_STORAGE_COLUMN)
                    .cloned()
                    .unwrap_or(Value::Null))
            } else {
                Ok(materialized.get(column).cloned().unwrap_or(Value::Null))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::from_values(values),
    ))
}

struct ViewCheckContext<'a> {
    engine: &'a Engine,
    table: &'a str,
    storage_table: &'a str,
    target_qualifier: &'a str,
    doc_id: DocId,
    document: &'a Document,
    checks: &'a [ViewCheckPlan],
    params: &'a [SQLParam],
    scope: &'a CteScope,
}

fn validate_view_checks(context: ViewCheckContext<'_>) -> Result<(), SQLError> {
    let ViewCheckContext {
        engine,
        table,
        storage_table,
        target_qualifier,
        doc_id,
        document,
        checks,
        params,
        scope,
    } = context;
    if checks.is_empty() {
        return Ok(());
    }
    let row = dml_target_row_for_storage(
        engine,
        table,
        storage_table,
        target_qualifier,
        doc_id,
        document,
    )?;
    for check in checks {
        let value = eval_mutation_expr(engine, scope, &check.predicate, Some(&row), params)?;
        if !uqa_sql::expr::truthy(&value) {
            return Err(SQLError::Routine {
                sqlstate: "44000".into(),
                message: format!(
                    "new row violates check option for view \"{}\"",
                    RelationIdentity::from_legacy_name(&check.view)
                        .map_or_else(|_| check.view.clone(), |relation| relation.name)
                ),
            });
        }
    }
    Ok(())
}

fn dml_null_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut columns = if definitions.is_empty() {
        engine
            .try_table_columns(table)
            .map_err(|error| dml_storage_error("DML row schema lookup", error))?
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(ColumnType::Oid));
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    let width = columns.len();
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::nulls(width),
    ))
}

fn dml_join_rows(left: &OwnedPhysicalRow, right: &OwnedPhysicalRow) -> OwnedPhysicalRow {
    OwnedPhysicalRow::new(
        RowSchema::join(&left.schema, &right.schema, std::iter::empty()),
        PhysicalRow::concat(&left.row, &right.row),
    )
}

fn validate_dml_expression_qualifiers(
    expression: &ScalarExpr,
    allowed: &BTreeSet<String>,
) -> Result<(), SQLError> {
    for qualifier in crate::sql::select::expr_qualifiers(expression) {
        if !allowed.contains(&qualifier) {
            return Err(SQLError::UnknownTable(qualifier));
        }
    }
    Ok(())
}

fn dml_append_hidden_qualified_row(
    base: &OwnedPhysicalRow,
    qualifier: &str,
    columns: &[String],
    types: &[Option<ColumnType>],
    values: Vec<Value>,
) -> OwnedPhysicalRow {
    let hidden_types = columns
        .iter()
        .enumerate()
        .map(|(position, _)| types.get(position).cloned().flatten())
        .collect::<Vec<_>>();
    let schema = RowSchema::append_hidden_typed(&base.schema, &hidden_types);
    let offset = base.schema.physical_width();
    let aliases = columns
        .iter()
        .enumerate()
        .map(|(position, column)| {
            (
                ColumnIdentity::qualified(qualifier, column),
                offset + position,
                hidden_types[position].clone(),
            )
        })
        .collect::<Vec<_>>();
    OwnedPhysicalRow::new(
        RowSchema::with_physical_identity_aliases(&schema, &aliases),
        base.row.clone().append_values(values),
    )
}

fn insert_identity_columns(
    engine: &Engine,
    table: &str,
    action: &str,
) -> Result<(Option<String>, String, bool), SQLError> {
    let auto_increment = engine
        .auto_increment_column(table)
        .map_err(|err| dml_storage_error(action, err))?;
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let primary_keys = definitions
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let primary_key = (primary_keys.len() == 1).then(|| primary_keys[0].clone());
    let accepts_supplied_identity =
        auto_increment.is_some() || primary_key.is_some() || definitions.is_empty();
    let id_column = auto_increment
        .clone()
        .or(primary_key)
        .unwrap_or_else(|| "id".into());
    // The conventional `id` field is a storage identity only for the legacy
    // schema-less document API. A declared SQL table without an identity or
    // primary key may contain duplicate ordinary `id` values, so using that
    // field as the physical document key would silently replace rows.
    Ok((auto_increment, id_column, accepts_supplied_identity))
}

fn prepare_auto_increment_identity(
    engine: &Engine,
    table: &str,
    id_column: &str,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<Option<(DocId, bool)>, SQLError> {
    let Some(auto_id_column) = auto_id_column else {
        return Ok(None);
    };
    let definitions = engine
        .auto_increment_columns(table)
        .map_err(|error| dml_storage_error(action, error))?;
    let mut selected_generated = false;
    for (column, provenance) in &definitions {
        if !provenance.is_identity() {
            continue;
        }
        let supplied = document
            .get(column)
            .is_some_and(|value| !matches!(value, Value::Null));
        if supplied {
            if provenance.kind == uqa_sql::ast::AutoIncrementKind::IdentityAlways {
                return Err(SQLError::Routine {
                    sqlstate: "428C9".into(),
                    message: format!(
                        "cannot insert a non-DEFAULT value into identity column \"{column}\""
                    ),
                });
            }
            continue;
        }
        let sequence = provenance.sequence.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "identity column `{table}.{column}` has no durable sequence binding"
            ))
        })?;
        let value = engine.nextval_sql(sequence)?;
        document.insert(
            column.clone(),
            crate::sql::coerce_to_column_type(engine, table, column, Value::Int(value))?,
        );
        selected_generated |= column == auto_id_column;
    }
    let provenance = definitions
        .iter()
        .find(|(column, _)| column == auto_id_column)
        .map(|(_, provenance)| provenance)
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "auto-increment column `{table}.{auto_id_column}` disappeared"
            ))
        })?;
    match provenance.kind {
        uqa_sql::ast::AutoIncrementKind::Serial => Ok(None),
        uqa_sql::ast::AutoIncrementKind::IdentityAlways
        | uqa_sql::ast::AutoIncrementKind::IdentityByDefault => {
            let mut identity = prepare_insert_identity(
                engine,
                table,
                id_column,
                true,
                Some(auto_id_column),
                document,
                action,
            )?;
            if selected_generated {
                identity.1 = false;
            }
            Ok(Some(identity))
        }
        uqa_sql::ast::AutoIncrementKind::Legacy => {
            let owner = engine.partition_identity_owner(table)?;
            engine.lock_relation(&owner, crate::row_locks::RelationLockMode::RowExclusive)?;
            prepare_insert_identity(
                engine,
                &owner,
                id_column,
                true,
                Some(auto_id_column),
                document,
                action,
            )
            .map(Some)
        }
    }
}

fn persist_auto_increment_identity(
    engine: &Engine,
    table: &str,
    auto_id_column: Option<&str>,
    action: &str,
) -> Result<(), SQLError> {
    let Some(auto_id_column) = auto_id_column else {
        return Ok(());
    };
    let legacy = engine
        .auto_increment_columns(table)
        .map_err(|error| dml_storage_error(action, error))?
        .into_iter()
        .find(|(column, _)| column == auto_id_column)
        .is_some_and(|(_, provenance)| provenance.kind == uqa_sql::ast::AutoIncrementKind::Legacy);
    if !legacy {
        return Ok(());
    }
    let owner = engine.partition_identity_owner(table)?;
    engine
        .persist_next_id(&owner)
        .map_err(|error| dml_storage_error(action, error))
}

fn prepare_insert_identity(
    engine: &Engine,
    allocation_table: &str,
    id_column: &str,
    accepts_supplied_identity: bool,
    auto_id_column: Option<&str>,
    document: &mut Document,
    action: &str,
) -> Result<(DocId, bool), SQLError> {
    let supplied_id = if accepts_supplied_identity {
        document_supplied_id(document, id_column, auto_id_column == Some(id_column))?
    } else {
        None
    };
    let supplied = supplied_id.is_some();
    let doc_id = match supplied_id {
        Some(doc_id) => doc_id,
        None => engine.allocate_next_id(allocation_table)?,
    };
    if auto_id_column == Some(id_column) {
        document.insert(id_column.to_string(), doc_id_value(doc_id)?);
    }
    engine
        .advance_next_id(allocation_table, doc_id)
        .map_err(|error| dml_storage_error(action, error))?;
    Ok((doc_id, supplied))
}

fn validate_mutation_columns<'a>(
    engine: &Engine,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    // Programmatically-created document tables intentionally have no SQL
    // schema and retain their open-field behavior. SQL CREATE TABLE always
    // supplies definitions, and those targets must reject misspelled or
    // repeated mutation columns instead of persisting arbitrary fields.
    if definitions.is_empty() {
        return Ok(());
    }
    let known: BTreeSet<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column) {
            return Err(SQLError::TypeMismatch(format!(
                "{action}: column `{column}` is specified more than once"
            )));
        }
        if !known.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{table}.{column}")));
        }
    }
    Ok(())
}

fn eval_mutation_expr(
    engine: &Engine,
    ctes: &CteScope,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let empty_schema = RowSchema::default();
    let schema = row.map_or(&empty_schema, |row| &row.schema);
    uqa_execution::scalar_type_with_resolver(expression, schema, params, engine)?;
    let expression = uqa_execution::bind_type_introspection_with_resolver(
        expression.clone(),
        schema,
        params,
        engine,
    );
    if let Some(row) = row {
        let view = row.view();
        let context = PhysicalEvalContext::from_row_lookup(&view, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook)
            .with_physical_outer_row(&row.schema, &row.row);
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    } else {
        let context = PhysicalEvalContext::new(None, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    }
}

fn row_independent_mutation_qualification_count(
    engine: &Engine,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<usize>, SQLError> {
    let Some(predicate) = predicate else {
        return Ok(Some(1));
    };
    let mut columns = BTreeSet::new();
    if !predicate.collect_columns(&mut columns) || !columns.is_empty() {
        return Ok(None);
    }
    Ok(Some(usize::from(uqa_sql::expr::truthy(
        &eval_mutation_expr(engine, ctes, predicate, None, params)?,
    ))))
}

struct MutationAssignmentTarget<'a> {
    table: &'a str,
    column: &'a str,
    action: &'a str,
}

fn eval_mutation_assignment(
    engine: &Engine,
    ctes: &CteScope,
    target: MutationAssignmentTarget<'_>,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let MutationAssignmentTarget {
        table,
        column,
        action,
    } = target;
    let generated = crate::sql::generated::generated_column_kind(engine, table, column)?;
    if matches!(expression, ScalarExpr::Default) {
        if generated.is_some() {
            return Ok(None);
        }
        let value = match engine
            .try_column_default_expr(table, column)
            .map_err(|error| dml_storage_error(action, error))?
        {
            Some(default) => eval_lowered_expression(engine, &default, None, params)?,
            None => Value::Null,
        };
        return coerce_to_column_type(engine, table, column, value).map(Some);
    }
    if generated.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "column `{column}` is a generated column; only DEFAULT may be assigned"
        )));
    }
    let value = eval_mutation_expr(engine, ctes, expression, row, params)?;
    coerce_to_column_type(engine, table, column, value).map(Some)
}

fn eval_view_rule_update_assignment(
    engine: &Engine,
    ctes: &CteScope,
    stmt: &UpdatePlan,
    assignment_position: usize,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let value = if matches!(expression, ScalarExpr::Default) {
        Value::Null
    } else {
        eval_mutation_expr(engine, ctes, expression, row, params)?
    };
    for plan in &stmt.view_rule_update_plans {
        let Some(column) = plan.assigned_columns.get(assignment_position) else {
            continue;
        };
        let definition = engine
            .view_definition(&plan.relation)?
            .ok_or_else(|| SQLError::UnknownTable(plan.relation.clone()))?;
        let schema = engine.stored_view_schema(&definition)?;
        let Some(position) =
            schema
                .columns()
                .iter()
                .enumerate()
                .find_map(|(position, internal)| {
                    let public = schema.public_name(position).unwrap_or(internal);
                    public.eq_ignore_ascii_case(column).then_some(position)
                })
        else {
            return Err(SQLError::UnknownColumn(format!(
                "{}.{}",
                plan.relation, column
            )));
        };
        return match schema.column_type(position) {
            Some(ty) => crate::sql::convert_value_to_column_type(value, ty).map(Some),
            None => Ok(Some(value)),
        };
    }
    Ok(Some(value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergePairKind {
    Matched,
    NotMatchedBySource,
    NotMatchedByTarget,
}

impl MergePairKind {
    fn encode(self) -> i64 {
        match self {
            Self::Matched => 0,
            Self::NotMatchedBySource => 1,
            Self::NotMatchedByTarget => 2,
        }
    }

    fn decode(value: &Value) -> Result<Self, SQLError> {
        match value {
            Value::Int(0) => Ok(Self::Matched),
            Value::Int(1) => Ok(Self::NotMatchedBySource),
            Value::Int(2) => Ok(Self::NotMatchedByTarget),
            other => Err(SQLError::Internal(format!(
                "invalid spilled MERGE pairing kind {other:?}"
            ))),
        }
    }
}

struct MergePairing {
    kind: MergePairKind,
    storage_table: Option<String>,
    doc_id: Option<DocId>,
    target_document: Option<Document>,
    source_row: OwnedPhysicalRow,
}

fn merge_pair_schema(source: &RowSchema) -> RowSchema {
    let header = RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![
            Some(ColumnType::BigInteger),
            Some(ColumnType::Text),
            Some(ColumnType::Text),
            None,
        ],
    );
    RowSchema::join(&header, source, std::iter::empty())
}

fn encode_merge_pair(
    kind: MergePairKind,
    storage_table: Option<&str>,
    doc_id: Option<DocId>,
    target_document: Option<&Document>,
    source_row: &OwnedPhysicalRow,
) -> uqa_execution::PhysicalRow {
    let header = PhysicalRow::from_values(vec![
        Value::Int(kind.encode()),
        storage_table.map_or(Value::Null, |table| Value::Str(table.to_string())),
        doc_id.map_or(Value::Null, |doc_id| Value::Str(doc_id.to_string())),
        target_document.map_or(Value::Null, |document| Value::Map(document.clone())),
    ]);
    PhysicalRow::concat(&header, &source_row.row)
}

fn decode_merge_pair(encoded: OwnedPhysicalRow) -> Result<MergePairing, SQLError> {
    let kind =
        MergePairKind::decode(encoded.physical_value_at(0).ok_or_else(|| {
            SQLError::Internal("spilled MERGE pairing lost its match kind".into())
        })?)?;
    let storage_table = match encoded.physical_value_at(1) {
        Some(Value::Str(table)) => Some(table.clone()),
        Some(Value::Null) | None => None,
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE storage table value {value:?}"
            )))
        }
    };
    let doc_id = match encoded.physical_value_at(2) {
        Some(Value::Null) | None => None,
        Some(Value::Str(doc_id)) => Some(doc_id.parse::<DocId>().map_err(|error| {
            SQLError::Internal(format!(
                "invalid spilled MERGE document id `{doc_id}`: {error}"
            ))
        })?),
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE document id value {value:?}"
            )))
        }
    };
    let target_document = match encoded.physical_value_at(3) {
        Some(Value::Map(document)) => Some(document.clone()),
        Some(Value::Null) | None => None,
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE target document value {value:?}"
            )))
        }
    };
    match kind {
        MergePairKind::Matched | MergePairKind::NotMatchedBySource
            if storage_table.is_none() || doc_id.is_none() || target_document.is_none() =>
        {
            return Err(SQLError::Internal(
                "target-bearing MERGE pairing lost its target row".into(),
            ));
        }
        MergePairKind::NotMatchedByTarget
            if storage_table.is_some() || doc_id.is_some() || target_document.is_some() =>
        {
            return Err(SQLError::Internal(
                "target-missing MERGE pairing unexpectedly retained a target row".into(),
            ));
        }
        _ => {}
    }
    Ok(MergePairing {
        kind,
        storage_table,
        doc_id,
        target_document,
        source_row: encoded,
    })
}

fn merge_source_index_value(index: usize) -> Value {
    Value::Str(index.to_string())
}

mod conflict;
mod constraints;
mod delete;
mod insert;
mod merge;
mod update;
mod update_from;
mod vectors;
pub(in crate::sql) mod view_automatic;
mod view_triggers;

pub(in crate::sql) use conflict::*;
pub(crate) use constraints::*;
pub(in crate::sql) use delete::*;
pub(in crate::sql) use insert::*;
pub(in crate::sql) use merge::*;
pub(in crate::sql) use update::*;
pub(in crate::sql) use update_from::*;
pub(in crate::sql) use vectors::*;
