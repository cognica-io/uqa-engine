//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` rewrite-rule qualification, OLD/NEW binding, action ordering, and recursion checks.

mod actions;
mod returning;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, Value};
use uqa_sql::ast::{BinaryOp, Expr, RuleEvent, Statement};
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::{Engine, RelationIdentity};

pub(in crate::sql) use super::dml::RuleRowImage;
use actions::{bind_insert_values_action, bind_set_oriented_action};
pub(in crate::sql) use returning::RuleReturningRequest;
use returning::{
    augment_rule_returning_action, capture_rule_returning_result, clear_statement_returning,
    rule_returning_columns, statement_has_returning, validate_rule_returning_provider_width,
};
pub(in crate::sql) use returning::{validate_rule_returning_contract, RuleReturningResult};

use super::scalar::{eval_lowered_expression, eval_stored_expression_plan_with_row};

thread_local! {
    static RULE_EXECUTION_STACK: RefCell<Vec<(String, RuleEvent)>> = const { RefCell::new(Vec::new()) };
}

struct PreparedRule {
    rule: crate::engine_events::StoredRule,
    matching_rows: Vec<usize>,
    condition_references_row: bool,
    action_references_row: Vec<bool>,
    action_row_columns: Vec<BTreeSet<String>>,
    action_columns: Vec<BTreeSet<String>>,
}

struct RuleColumnMetadata {
    ty: uqa_sql::ast::ColumnType,
    uses_document_id: bool,
    position: usize,
}

fn rule_condition_plan_references_row(plan: &uqa_planner::ExpressionPlan) -> bool {
    crate::engine_events::rule_condition_plan_references_whole_row(plan)
        || !crate::engine_events::rule_condition_plan_row_columns(plan).is_empty()
}

pub(in crate::sql) struct PreparedRuleBatch {
    table: String,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
    rules: Vec<PreparedRule>,
    suppress_original: Vec<bool>,
    action_qualification_count: Option<usize>,
}

pub(in crate::sql) struct RuleExecutionOutcome {
    pub(in crate::sql) returning: Option<RuleReturningResult>,
    pub(in crate::sql) affected_rows: u64,
    pub(in crate::sql) executed_action: bool,
}

#[derive(Clone, Copy)]
pub(in crate::sql) enum RuleRowSide {
    Old,
    New,
}

impl PreparedRuleBatch {
    pub(in crate::sql) fn event_row_count(&self) -> usize {
        self.rows.len()
    }

    pub(in crate::sql) fn set_action_qualification_count(&mut self, count: usize) {
        debug_assert!(matches!(self.event, RuleEvent::Update | RuleEvent::Delete));
        self.action_qualification_count = Some(count);
    }

    pub(in crate::sql) fn suppresses(&self, index: usize) -> bool {
        self.suppress_original.get(index).copied().unwrap_or(false)
    }

    pub(in crate::sql) fn matched_action_row_columns(&self) -> Vec<BTreeSet<String>> {
        let mut rows = vec![BTreeSet::new(); self.rows.len()];
        for rule in &self.rules {
            for columns in &rule.action_row_columns {
                for row in &rule.matching_rows {
                    if let Some(required) = rows.get_mut(*row) {
                        required.extend(columns.iter().cloned());
                    }
                }
            }
        }
        rows
    }

    pub(in crate::sql) fn missing_action_row_columns(
        &self,
    ) -> Vec<(BTreeSet<String>, BTreeSet<String>)> {
        self.matched_action_row_columns()
            .into_iter()
            .zip(&self.rows)
            .map(|(columns, row)| {
                let missing = |record: &Option<Document>| {
                    if let Some(record) = record {
                        columns
                            .iter()
                            .filter(|column| !record.contains_key(*column))
                            .cloned()
                            .collect()
                    } else {
                        BTreeSet::new()
                    }
                };
                (missing(&row.old), missing(&row.new))
            })
            .collect()
    }

    pub(in crate::sql) fn supplement_rows(
        &mut self,
        rows: Vec<RuleRowImage>,
    ) -> Result<(), SQLError> {
        if rows.len() != self.rows.len() {
            return Err(SQLError::Internal(
                "rewrite-rule supplemental row count changed after qualification".into(),
            ));
        }
        for (target, supplemental) in self.rows.iter_mut().zip(rows) {
            target.supplement_documents(supplemental);
        }
        Ok(())
    }

    pub(in crate::sql) fn execute_actions(
        &self,
        engine: &Engine,
        request: RuleReturningRequest,
    ) -> Result<Option<RuleReturningResult>, SQLError> {
        Ok(self
            .execute_actions_with_affected(engine, request)?
            .returning)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves action and RETURNING order"
    )]
    pub(in crate::sql) fn execute_actions_with_affected(
        &self,
        engine: &Engine,
        request: RuleReturningRequest,
    ) -> Result<RuleExecutionOutcome, SQLError> {
        if self.rules.is_empty() {
            return Ok(RuleExecutionOutcome {
                returning: None,
                affected_rows: 0,
                executed_action: false,
            });
        }
        let privilege_subject = engine.rule_privilege_subject(&self.table)?;
        let _guard = RuleExecutionGuard::enter(&self.table, self.event)?;
        let columns = rule_columns(engine, &self.table)?;
        let returning_columns = request
            .captures()
            .then(|| rule_returning_columns(engine, &self.table))
            .transpose()?;
        let provider_exists = request.captures()
            && self.rules.iter().any(|prepared| {
                prepared
                    .rule
                    .definition
                    .actions
                    .iter()
                    .any(statement_has_returning)
            });
        let mut returning = provider_exists.then(RuleReturningResult::empty);
        let mut affected_rows = 0_u64;
        let mut executed_action = false;
        let mut provider_captured = false;
        for prepared in &self.rules {
            for (action_index, action) in prepared.rule.definition.actions.iter().enumerate() {
                let action_columns =
                    prepared.action_columns.get(action_index).ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite rule lost its prepared action column contract".into(),
                        )
                    })?;
                let mut reevaluated_rows;
                let reevaluated_matching_rows;
                let (action_matching_rows, action_rows) =
                    if self.event == RuleEvent::Insert && prepared.rule.condition_plan.is_some() {
                        reevaluated_rows = self.rows.clone();
                        let mut matched = Vec::new();
                        for (row_index, row) in reevaluated_rows.iter_mut().enumerate() {
                            if rule_condition_matches(
                                engine,
                                &prepared.rule,
                                &privilege_subject,
                                row_index,
                                row,
                                &columns,
                                &mut |_, _, _| Ok(None),
                            )? {
                                matched.push(row_index);
                            }
                        }
                        reevaluated_matching_rows = matched;
                        (
                            reevaluated_matching_rows.as_slice(),
                            reevaluated_rows.as_slice(),
                        )
                    } else {
                        (prepared.matching_rows.as_slice(), self.rows.as_slice())
                    };
                let action_returns = statement_has_returning(action);
                let captures_action = request.captures() && action_returns;
                let captures_source_context = captures_action
                    && matches!(action, Statement::Update(_) | Statement::Delete(_))
                    && action_matching_rows.iter().any(|row_index| {
                        action_rows
                            .get(*row_index)
                            .is_some_and(|row| row.context.is_some())
                    });
                let action_references_row = prepared
                    .action_references_row
                    .get(action_index)
                    .copied()
                    .unwrap_or(false);
                let uses_action_qualification =
                    matches!(self.event, RuleEvent::Update | RuleEvent::Delete)
                        && !prepared.condition_references_row
                        && !action_references_row
                        && !captures_source_context
                        && self.action_qualification_count.is_some();
                let qualification_rows;
                let qualification_indices;
                let (matching_rows, rows) = if uses_action_qualification {
                    let count = self.action_qualification_count.unwrap_or_default();
                    let mut matched = Vec::with_capacity(count);
                    for qualification_index in 0..count {
                        let mut row = RuleRowImage::empty();
                        if rule_condition_matches(
                            engine,
                            &prepared.rule,
                            &privilege_subject,
                            qualification_index,
                            &mut row,
                            &columns,
                            &mut |_, _, _| Ok(None),
                        )? {
                            matched.push(row);
                        }
                    }
                    qualification_rows = matched;
                    qualification_indices = (0..qualification_rows.len()).collect::<Vec<_>>();
                    (
                        qualification_indices.as_slice(),
                        qualification_rows.as_slice(),
                    )
                } else {
                    (action_matching_rows, action_rows)
                };
                let needs_row_source = self.event == RuleEvent::Insert
                    || prepared.condition_references_row
                    || action_references_row
                    || captures_source_context
                    || uses_action_qualification;
                if needs_row_source
                    && matching_rows.len() > 1
                    && crate::engine_events::rule_action_has_set_operation(action)
                {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message:
                            "conditional UNION/INTERSECT/EXCEPT statements are not implemented"
                                .into(),
                    });
                }
                let (mut bound, source_index) = if needs_row_source
                    && matches!(action, Statement::Insert(insert) if !insert.rows.is_empty())
                {
                    (
                        bind_insert_values_action(
                            engine,
                            action,
                            matching_rows,
                            rows,
                            &columns,
                            action_columns,
                        )?,
                        None,
                    )
                } else if needs_row_source {
                    let bound = bind_set_oriented_action(
                        engine,
                        action,
                        matching_rows,
                        rows,
                        &columns,
                        action_columns,
                    )?;
                    let source_index = captures_source_context.then_some(bound.source_index);
                    (bound.statement, source_index)
                } else {
                    (action.clone(), None)
                };
                if captures_action {
                    if provider_captured {
                        return Err(SQLError::Routine {
                            sqlstate: "0A000".into(),
                            message: "cannot have RETURNING lists in multiple rules".into(),
                        });
                    }
                    provider_captured = true;
                    let event_width = returning_columns.as_ref().map_or(0, Vec::len);
                    let provider_width =
                        crate::sql::analyze_rule_action_returning_schema(engine, bound.clone())?
                            .ok_or_else(|| {
                                SQLError::Internal(
                                    "rewrite-rule RETURNING provider lost its declared row type"
                                        .into(),
                                )
                            })?
                            .len();
                    validate_rule_returning_provider_width(provider_width, event_width)?;
                    augment_rule_returning_action(
                        &mut bound,
                        source_index,
                        event_width,
                        request,
                        action_columns,
                    )?;
                } else if action_returns {
                    clear_statement_returning(&mut bound);
                }
                let result = super::execute_compiled_statement_with_privilege_subject(
                    engine,
                    bound,
                    &[],
                    &privilege_subject,
                )?;
                affected_rows = result.affected_rows;
                executed_action = true;
                if captures_action {
                    let definitions = returning_columns.as_deref().ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING capture lost the event row type".into(),
                        )
                    })?;
                    let captured = capture_rule_returning_result(
                        engine,
                        result,
                        definitions,
                        captures_source_context.then_some(self.rows.as_slice()),
                    )?;
                    returning = Some(captured);
                }
            }
        }
        Ok(RuleExecutionOutcome {
            returning,
            affected_rows,
            executed_action,
        })
    }
}

pub(in crate::sql) fn prepare_rule_batch(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
) -> Result<PreparedRuleBatch, SQLError> {
    prepare_rule_batch_with_projection(engine, table, event, rows, |_, _, _| Ok(None))
}

pub(in crate::sql) fn prepare_rule_batch_with_projection<F>(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
    mut rows: Vec<RuleRowImage>,
    mut project: F,
) -> Result<PreparedRuleBatch, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let table = engine.resolve_rule_relation(table)?.qualified_name();
    let rules = engine.rules_for(&table, event)?;
    if rules.is_empty() {
        return Ok(PreparedRuleBatch {
            table,
            event,
            suppress_original: vec![false; rows.len()],
            rows,
            rules: Vec::new(),
            action_qualification_count: None,
        });
    }
    ensure_not_recursive(&table, event)?;
    let columns = rule_columns(engine, &table)?;
    let privilege_subject = engine.rule_privilege_subject(&table)?;
    let mut suppress_original = vec![false; rows.len()];
    let mut prepared_rules = Vec::with_capacity(rules.len());
    for rule in rules {
        let condition_references_row = if let Some(plan) = rule.condition_plan.as_ref() {
            rule_condition_plan_references_row(plan)
        } else {
            rule.definition
                .condition
                .as_ref()
                .is_some_and(crate::engine_events::rule_expr_references_row)
        };
        let action_columns = rule
            .definition
            .actions
            .iter()
            .map(|action| engine.rule_action_target_columns(action))
            .collect::<Result<Vec<_>, SQLError>>()?;
        let action_references_row = rule
            .definition
            .actions
            .iter()
            .zip(&action_columns)
            .map(|(action, columns)| {
                crate::engine_events::rule_statement_references_row(engine, action, columns)
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let action_row_columns = rule
            .definition
            .actions
            .iter()
            .zip(&action_columns)
            .map(|(action, action_columns)| {
                if crate::engine_events::rule_statement_references_whole_row(
                    engine,
                    action,
                    action_columns,
                )? {
                    Ok(columns.keys().cloned().collect())
                } else {
                    crate::engine_events::rule_statement_row_columns(engine, action, action_columns)
                }
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let mut matching_rows = Vec::new();
        for (index, row) in rows.iter_mut().enumerate() {
            if rule_condition_matches(
                engine,
                &rule,
                &privilege_subject,
                index,
                row,
                &columns,
                &mut project,
            )? {
                matching_rows.push(index);
                if rule.definition.instead {
                    suppress_original[index] = true;
                }
            }
        }
        prepared_rules.push(PreparedRule {
            rule,
            matching_rows,
            condition_references_row,
            action_references_row,
            action_row_columns,
            action_columns,
        });
    }
    Ok(PreparedRuleBatch {
        table,
        event,
        rows,
        rules: prepared_rules,
        suppress_original,
        action_qualification_count: None,
    })
}

pub(in crate::sql) fn relation_has_returning_provider(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    Ok(engine
        .rules_for(table, event)?
        .iter()
        .any(|rule| rule.definition.actions.iter().any(statement_has_returning)))
}

pub(in crate::sql) fn relation_suppresses_original_query(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    Ok(engine
        .rules_for(table, event)?
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
}

pub(in crate::sql) fn relation_rules_reference_row(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    for rule in engine.rules_for(table, event)? {
        let condition_references_row = if let Some(plan) = rule.condition_plan.as_ref() {
            rule_condition_plan_references_row(plan)
        } else {
            rule.definition
                .condition
                .as_ref()
                .is_some_and(crate::engine_events::rule_expr_references_row)
        };
        if condition_references_row {
            return Ok(true);
        }
        for action in &rule.definition.actions {
            let target_columns = engine.rule_action_target_columns(action)?;
            if crate::engine_events::rule_statement_references_row(engine, action, &target_columns)?
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(in crate::sql) fn relation_rules_require_event_rows(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    Ok(engine
        .rules_for(table, event)?
        .iter()
        .any(|rule| rule.definition.condition.is_some() || !rule.definition.actions.is_empty()))
}

pub(in crate::sql) fn surviving_view_rules_reference_row(
    engine: &Engine,
    relations: &[String],
    event: RuleEvent,
) -> Result<bool, SQLError> {
    let mut references_row = false;
    for relation in relations {
        references_row |= relation_rules_reference_row(engine, relation, event)?;
        if relation_suppresses_original_query(engine, relation, event)? {
            break;
        }
    }
    Ok(references_row)
}

pub(in crate::sql) fn surviving_view_rules_require_event_rows(
    engine: &Engine,
    relations: &[String],
    event: RuleEvent,
) -> Result<bool, SQLError> {
    let mut requires_rows = false;
    for relation in relations {
        requires_rows |= relation_rules_require_event_rows(engine, relation, event)?;
        if relation_suppresses_original_query(engine, relation, event)? {
            break;
        }
    }
    Ok(requires_rows)
}

pub(in crate::sql) fn relation_condition_row_columns(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<BTreeSet<String>, SQLError> {
    let mut columns = BTreeSet::new();
    for rule in engine.rules_for(table, event)? {
        if let Some(plan) = rule.condition_plan.as_ref() {
            columns.extend(crate::engine_events::rule_condition_plan_row_columns(plan));
            continue;
        }
        if let Some(condition) = rule.definition.condition.as_ref() {
            columns.extend(crate::engine_events::rule_expr_row_columns(condition));
        }
    }
    Ok(columns)
}

pub(in crate::sql) fn relation_rule_row_columns(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    let mut columns = BTreeSet::new();
    let mut references_row = false;
    let mut references_whole_row = false;
    for rule in engine.rules_for(table, event)? {
        if let Some(plan) = rule.condition_plan.as_ref() {
            let plan_references_row = rule_condition_plan_references_row(plan);
            references_row |= plan_references_row;
            references_whole_row |=
                crate::engine_events::rule_condition_plan_references_whole_row(plan);
            columns.extend(crate::engine_events::rule_condition_plan_row_columns(plan));
        } else if let Some(condition) = rule.definition.condition.as_ref() {
            references_row |= crate::engine_events::rule_expr_references_row(condition);
            references_whole_row |= crate::engine_events::rule_expr_references_whole_row(condition);
            columns.extend(crate::engine_events::rule_expr_row_columns(condition));
        }
        for action in &rule.definition.actions {
            let action_columns = engine.rule_action_target_columns(action)?;
            references_row |= crate::engine_events::rule_statement_references_row(
                engine,
                action,
                &action_columns,
            )?;
            references_whole_row |= crate::engine_events::rule_statement_references_whole_row(
                engine,
                action,
                &action_columns,
            )?;
            columns.extend(crate::engine_events::rule_statement_row_columns(
                engine,
                action,
                &action_columns,
            )?);
        }
    }
    if references_whole_row || references_row && columns.is_empty() {
        Ok(None)
    } else {
        Ok(Some(columns))
    }
}

struct RuleExecutionGuard {
    table: String,
    event: RuleEvent,
}

impl RuleExecutionGuard {
    fn enter(table: &str, event: RuleEvent) -> Result<Self, SQLError> {
        ensure_not_recursive(table, event)?;
        RULE_EXECUTION_STACK.with(|stack| stack.borrow_mut().push((table.to_string(), event)));
        Ok(Self {
            table: table.to_string(),
            event,
        })
    }
}

impl Drop for RuleExecutionGuard {
    fn drop(&mut self) {
        RULE_EXECUTION_STACK.with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped, Some((self.table.clone(), self.event)));
        });
    }
}

fn ensure_not_recursive(table: &str, event: RuleEvent) -> Result<(), SQLError> {
    let recursive = RULE_EXECUTION_STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .any(|(active_table, active_event)| active_table == table && *active_event == event)
    });
    if !recursive {
        return Ok(());
    }
    let relation = RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("decode rule relation `{table}`: {error}")))?;
    Err(SQLError::Routine {
        sqlstate: "42P17".into(),
        message: format!(
            "infinite recursion detected in rules for relation \"{}\"",
            relation.name
        ),
    })
}

fn rule_columns(
    engine: &Engine,
    table: &str,
) -> Result<BTreeMap<String, RuleColumnMetadata>, SQLError> {
    let columns = engine
        .try_describe_table_row_type(table)
        .map_err(|error| SQLError::Internal(format!("read rule row type: {error}")))?;
    if let Some(columns) = columns {
        return Ok(columns
            .into_iter()
            .enumerate()
            .map(|(position, column)| {
                let uses_document_id = column.primary_key && column.ty.is_integer();
                (
                    column.name,
                    RuleColumnMetadata {
                        ty: column.ty,
                        uses_document_id,
                        position,
                    },
                )
            })
            .collect());
    }
    Ok(engine
        .rule_relation_columns(table)?
        .into_iter()
        .enumerate()
        .map(|(position, (name, ty))| {
            (
                name,
                RuleColumnMetadata {
                    ty,
                    uses_document_id: false,
                    position,
                },
            )
        })
        .collect())
}

struct RuntimeRuleResolver<'a> {
    old: Option<&'a Document>,
    new: Option<&'a Document>,
    old_doc_id: Option<DocId>,
    new_doc_id: Option<DocId>,
    columns: &'a BTreeMap<String, RuleColumnMetadata>,
}

impl RuntimeRuleResolver<'_> {
    fn record_field(
        &self,
        record: Option<&Document>,
        doc_id: Option<DocId>,
        column: &str,
    ) -> Result<ResolvedVariable, SQLError> {
        let metadata = self
            .columns
            .get(column)
            .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?;
        let value = if let Some(value) = record.and_then(|record| record.get(column).cloned()) {
            value
        } else if metadata.uses_document_id {
            doc_id
                .map(i64::try_from)
                .transpose()
                .map_err(|_| {
                    SQLError::TypeMismatch("document id exceeds PostgreSQL bigint".into())
                })?
                .map_or(Value::Null, Value::Int)
        } else {
            Value::Null
        };
        Ok(ResolvedVariable {
            value,
            declared_type: Some(metadata.ty.sql_name()),
        })
    }

    fn record(
        &self,
        record: Option<&Document>,
        doc_id: Option<DocId>,
    ) -> Result<ResolvedVariable, SQLError> {
        let mut columns = self.columns.iter().collect::<Vec<_>>();
        columns.sort_by_key(|(_, metadata)| metadata.position);
        let fields = columns
            .into_iter()
            .map(|(column, _)| {
                self.record_field(record, doc_id, column)
                    .map(|field| (column.clone(), field.value))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedVariable::untyped(Value::Record(fields)))
    }
}

impl VariableResolver for RuntimeRuleResolver<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if name.eq_ignore_ascii_case("old") {
            return self.record(self.old, self.old_doc_id).map(Some);
        }
        if name.eq_ignore_ascii_case("new") {
            return self.record(self.new, self.new_doc_id).map(Some);
        }
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier.eq_ignore_ascii_case("old") {
            return self
                .record_field(self.old, self.old_doc_id, column)
                .map(Some);
        }
        if qualifier.eq_ignore_ascii_case("new") {
            return self
                .record_field(self.new, self.new_doc_id, column)
                .map(Some);
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .resolve_name(qualifier)?
            .map(|record| Expr::Literal(record.value)))
    }
}

struct ProjectedRuntimeRuleResolver<'a, F> {
    row_index: usize,
    row: &'a mut RuleRowImage,
    columns: &'a BTreeMap<String, RuleColumnMetadata>,
    project: &'a mut F,
}

impl<F> ProjectedRuntimeRuleResolver<'_, F>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    fn record_field(
        &mut self,
        side: RuleRowSide,
        column: &str,
    ) -> Result<ResolvedVariable, SQLError> {
        let metadata = self
            .columns
            .get(column)
            .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?;
        let (record, doc_id) = match side {
            RuleRowSide::Old => (&mut self.row.old, self.row.old_doc_id),
            RuleRowSide::New => (&mut self.row.new, self.row.new_doc_id),
        };
        let value = if let Some(value) = record
            .as_ref()
            .and_then(|record| record.get(column).cloned())
        {
            value
        } else if metadata.uses_document_id {
            doc_id
                .map(i64::try_from)
                .transpose()
                .map_err(|_| {
                    SQLError::TypeMismatch("document id exceeds PostgreSQL bigint".into())
                })?
                .map_or(Value::Null, Value::Int)
        } else if record.is_some() {
            let value = (self.project)(self.row_index, side, column)?.unwrap_or(Value::Null);
            if let Some(record) = record.as_mut() {
                record.insert(column.to_string(), value.clone());
            }
            value
        } else {
            Value::Null
        };
        Ok(ResolvedVariable {
            value,
            declared_type: Some(metadata.ty.sql_name()),
        })
    }

    fn record(&mut self, side: RuleRowSide) -> Result<ResolvedVariable, SQLError> {
        let mut columns = self
            .columns
            .iter()
            .map(|(column, metadata)| (column.clone(), metadata.position))
            .collect::<Vec<_>>();
        columns.sort_by_key(|(_, position)| *position);
        let fields = columns
            .into_iter()
            .map(|(column, _)| {
                self.record_field(side, &column)
                    .map(|field| (column, field.value))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedVariable::untyped(Value::Record(fields)))
    }
}

impl<F> VariableResolver for ProjectedRuntimeRuleResolver<'_, F>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if name.eq_ignore_ascii_case("old") {
            return self.record(RuleRowSide::Old).map(Some);
        }
        if name.eq_ignore_ascii_case("new") {
            return self.record(RuleRowSide::New).map(Some);
        }
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier.eq_ignore_ascii_case("old") {
            return self.record_field(RuleRowSide::Old, column).map(Some);
        }
        if qualifier.eq_ignore_ascii_case("new") {
            return self.record_field(RuleRowSide::New, column).map(Some);
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .resolve_name(qualifier)?
            .map(|record| Expr::Literal(record.value)))
    }
}

mod condition_binding;
use condition_binding::rule_condition_matches;
