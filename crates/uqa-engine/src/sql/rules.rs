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
use uqa_execution::OwnedPhysicalRow;
use uqa_sql::ast::{BinaryOp, Expr, RuleEvent, Statement};
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::{Engine, RelationIdentity};

use actions::{bind_insert_values_action, bind_set_oriented_action};
pub(in crate::sql) use returning::RuleReturningRequest;
use returning::{
    augment_rule_returning_action, capture_rule_returning_result, clear_statement_returning,
    rule_returning_columns, statement_has_returning,
};
pub(in crate::sql) use returning::{validate_rule_returning_contract, RuleReturningResult};

use super::scalar::eval_lowered_expression;

thread_local! {
    static RULE_EXECUTION_STACK: RefCell<Vec<(String, RuleEvent)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
pub(in crate::sql) struct RuleRowImage {
    pub(in crate::sql) old_storage_table: Option<String>,
    pub(in crate::sql) old_doc_id: Option<DocId>,
    pub(in crate::sql) old: Option<Document>,
    pub(in crate::sql) new_storage_table: Option<String>,
    pub(in crate::sql) new_doc_id: Option<DocId>,
    pub(in crate::sql) new: Option<Document>,
    pub(in crate::sql) context: Option<OwnedPhysicalRow>,
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
            supplement_rule_document(&mut target.old, supplemental.old);
            supplement_rule_document(&mut target.new, supplemental.new);
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
                let action_returns = statement_has_returning(action);
                let captures_action = request.captures() && action_returns;
                let captures_source_context = captures_action
                    && matches!(action, Statement::Update(_) | Statement::Delete(_))
                    && prepared.matching_rows.iter().any(|row_index| {
                        self.rows
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
                        let mut row = empty_rule_row_image();
                        if rule_condition_matches(
                            engine,
                            prepared.rule.definition.condition.as_ref(),
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
                    (prepared.matching_rows.as_slice(), self.rows.as_slice())
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
                let result = super::execute_compiled_statement(engine, bound, &[])?;
                affected_rows = result.affected_rows;
                executed_action = true;
                if captures_action {
                    let definitions = returning_columns.as_deref().ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING capture lost the event row type".into(),
                        )
                    })?;
                    let captured = capture_rule_returning_result(
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
    let mut suppress_original = vec![false; rows.len()];
    let mut prepared_rules = Vec::with_capacity(rules.len());
    for rule in rules {
        let condition_references_row = rule
            .definition
            .condition
            .as_ref()
            .is_some_and(crate::engine_events::rule_expr_references_row);
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
                crate::engine_events::rule_statement_references_row(action, columns)
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let action_row_columns = rule
            .definition
            .actions
            .iter()
            .zip(&action_columns)
            .map(|(action, columns)| {
                crate::engine_events::rule_statement_row_columns(action, columns)
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let mut matching_rows = Vec::new();
        for (index, row) in rows.iter_mut().enumerate() {
            if rule_condition_matches(
                engine,
                rule.definition.condition.as_ref(),
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

fn empty_rule_row_image() -> RuleRowImage {
    RuleRowImage {
        old_storage_table: None,
        old_doc_id: None,
        old: None,
        new_storage_table: None,
        new_doc_id: None,
        new: None,
        context: None,
    }
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
        if rule
            .definition
            .condition
            .as_ref()
            .is_some_and(crate::engine_events::rule_expr_references_row)
        {
            return Ok(true);
        }
        for action in &rule.definition.actions {
            let target_columns = engine.rule_action_target_columns(action)?;
            if crate::engine_events::rule_statement_references_row(action, &target_columns)? {
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
    for rule in engine.rules_for(table, event)? {
        if let Some(condition) = rule.definition.condition.as_ref() {
            references_row |= crate::engine_events::rule_expr_references_row(condition);
            columns.extend(crate::engine_events::rule_expr_row_columns(condition));
        }
        for action in &rule.definition.actions {
            let action_columns = engine.rule_action_target_columns(action)?;
            references_row |=
                crate::engine_events::rule_statement_references_row(action, &action_columns)?;
            columns.extend(crate::engine_events::rule_statement_row_columns(
                action,
                &action_columns,
            )?);
        }
    }
    if references_row && columns.is_empty() {
        Ok(None)
    } else {
        Ok(Some(columns))
    }
}

fn supplement_rule_document(target: &mut Option<Document>, supplemental: Option<Document>) {
    let Some(supplemental) = supplemental else {
        return;
    };
    if let Some(target) = target {
        target.extend(supplemental);
    } else {
        *target = Some(supplemental);
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
            .map(|column| {
                let uses_document_id = column.primary_key && column.ty.is_integer();
                (
                    column.name,
                    RuleColumnMetadata {
                        ty: column.ty,
                        uses_document_id,
                    },
                )
            })
            .collect());
    }
    Ok(engine
        .rule_relation_columns(table)?
        .into_iter()
        .map(|(name, ty)| {
            (
                name,
                RuleColumnMetadata {
                    ty,
                    uses_document_id: false,
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
}

impl VariableResolver for RuntimeRuleResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
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
}

impl<F> VariableResolver for ProjectedRuntimeRuleResolver<'_, F>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
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
}

fn evaluate_rule_condition_piece<F>(
    engine: &Engine,
    expression: &Expr,
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<Value, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let bound = bind_rule_condition_expression(engine, expression, resolver)?;
    eval_lowered_expression(engine, &bound, None, &[])
}

fn bind_rule_condition_expressions<F>(
    engine: &Engine,
    expressions: &[Expr],
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<Vec<Expr>, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    expressions
        .iter()
        .map(|expression| bind_rule_condition_expression(engine, expression, resolver))
        .collect()
}

fn bind_rule_condition_expression<F>(
    engine: &Engine,
    expression: &Expr,
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<Expr, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    Ok(match expression {
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let base = base
                .as_deref()
                .map(|base| evaluate_rule_condition_piece(engine, base, resolver))
                .transpose()?;
            let mut selected = None;
            for (condition, result) in when {
                let condition = evaluate_rule_condition_piece(engine, condition, resolver)?;
                let matches = if let Some(base) = base.as_ref() {
                    matches!(
                        uqa_sql::expr::eval_binary_values(BinaryOp::Equal, base, &condition)?,
                        Value::Bool(true)
                    )
                } else {
                    uqa_sql::expr::truthy(&condition)
                };
                if matches {
                    selected = Some(bind_rule_condition_expression(engine, result, resolver)?);
                    break;
                }
            }
            if let Some(selected) = selected {
                selected
            } else if let Some(branch) = else_branch.as_deref() {
                bind_rule_condition_expression(engine, branch, resolver)?
            } else {
                Expr::Literal(Value::Null)
            }
        }
        Expr::And(items) => {
            let mut saw_null = false;
            let mut result = Value::Bool(true);
            for item in items {
                let value = evaluate_rule_condition_piece(engine, item, resolver)?;
                if matches!(value, Value::Null) {
                    saw_null = true;
                } else if !uqa_sql::expr::truthy(&value) {
                    result = Value::Bool(false);
                    saw_null = false;
                    break;
                }
            }
            if saw_null {
                result = Value::Null;
            }
            Expr::Literal(result)
        }
        Expr::Or(items) => {
            let mut saw_null = false;
            let mut result = Value::Bool(false);
            for item in items {
                let value = evaluate_rule_condition_piece(engine, item, resolver)?;
                if matches!(value, Value::Null) {
                    saw_null = true;
                } else if uqa_sql::expr::truthy(&value) {
                    result = Value::Bool(true);
                    saw_null = false;
                    break;
                }
            }
            if saw_null {
                result = Value::Null;
            }
            Expr::Literal(result)
        }
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => Expr::Func {
            name: name.clone(),
            binding: binding.clone(),
            args: bind_rule_condition_expressions(engine, args, resolver)?,
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    Ok(uqa_sql::ast::OrderBy {
                        expr: bind_rule_condition_expression(engine, &order.expr, resolver)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            filter: filter
                .as_deref()
                .map(|filter| {
                    bind_rule_condition_expression(engine, filter, resolver).map(Box::new)
                })
                .transpose()?,
        },
        Expr::Array(items) => {
            Expr::Array(bind_rule_condition_expressions(engine, items, resolver)?)
        }
        Expr::Row(items) => Expr::Row(bind_rule_condition_expressions(engine, items, resolver)?),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(bind_rule_condition_expression(engine, lhs, resolver)?),
            rhs: Box::new(bind_rule_condition_expression(engine, rhs, resolver)?),
        },
        Expr::UnaryMinus(inner) => Expr::UnaryMinus(Box::new(bind_rule_condition_expression(
            engine, inner, resolver,
        )?)),
        Expr::Not(inner) => Expr::Not(Box::new(bind_rule_condition_expression(
            engine, inner, resolver,
        )?)),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(bind_rule_condition_expression(engine, expr, resolver)?),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(bind_rule_condition_expression(engine, expr, resolver)?),
            low: Box::new(bind_rule_condition_expression(engine, low, resolver)?),
            high: Box::new(bind_rule_condition_expression(engine, high, resolver)?),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(bind_rule_condition_expression(engine, expr, resolver)?),
            list: bind_rule_condition_expressions(engine, list, resolver)?,
            negated: *negated,
        },
        Expr::Cast { expr, ty } => Expr::Cast {
            expr: Box::new(bind_rule_condition_expression(engine, expr, resolver)?),
            ty: ty.clone(),
        },
        Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => bind_expr(expression, resolver)?,
        Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Param(_)
        | Expr::InternalColumn(_)
        | Expr::Default
        | Expr::Literal(_)
        | Expr::Star
        | Expr::QualifiedStar(_) => bind_expr(expression, resolver)?,
    })
}

fn rule_condition_matches<F>(
    engine: &Engine,
    condition: Option<&Expr>,
    row_index: usize,
    row: &mut RuleRowImage,
    columns: &BTreeMap<String, RuleColumnMetadata>,
    project: &mut F,
) -> Result<bool, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let Some(condition) = condition else {
        return Ok(true);
    };
    let condition = bind_rule_condition_expression(
        engine,
        condition,
        &mut ProjectedRuntimeRuleResolver {
            row_index,
            row,
            columns,
            project,
        },
    )?;
    Ok(uqa_sql::expr::truthy(&eval_lowered_expression(
        engine,
        &condition,
        None,
        &[],
    )?))
}
